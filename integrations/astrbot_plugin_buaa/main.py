"""Boya commands and proactive QQ delivery for AstrBot 4.29.4."""

import asyncio
import json
from pathlib import Path
from types import SimpleNamespace

from astrbot.api import logger
from astrbot.api.event import AstrMessageEvent, filter
from astrbot.api.provider import ProviderRequest
from astrbot.api.star import Context, Star
from astrbot.core.platform.sources.qqofficial.qqofficial_message_event import (
    QQOfficialMessageEvent,
)


BRIDGE_DIR = Path("/AstrBot/data/.buaa-bridge")
COMMANDS = {"状态": "status", "已选": "selected", "统计": "statistics"}
TOOL = "query_buaa_boya"


class Boya(Star):
    def __init__(self, context: Context):
        super().__init__(context)
        self.owner = json.loads((BRIDGE_DIR / "owner.json").read_text())
        self.worker = None

    async def initialize(self):
        self.worker = asyncio.create_task(self.deliver())

    async def terminate(self):
        if self.worker:
            self.worker.cancel()
            await asyncio.gather(self.worker, return_exceptions=True)

    def allowed(self, event: AstrMessageEvent) -> bool:
        return (
            event.is_private_chat()
            and event.get_platform_name() == "qq_official"
            and event.get_platform_id() == self.owner["platform"]
            and event.get_sender_id() == self.owner["openid"]
            and event.is_admin()
        )

    async def query(self, operation: str) -> str:
        async def read_result():
            reader, writer = await asyncio.open_unix_connection(
                BRIDGE_DIR / "query.sock"
            )
            try:
                writer.write((operation + "\n").encode())
                await writer.drain()
                return json.loads(await reader.readline())["text"]
            finally:
                writer.close()
                await writer.wait_closed()

        try:
            return await asyncio.wait_for(read_result(), timeout=16)
        except (OSError, TimeoutError, ValueError, KeyError):
            return "暂时无法查询 Win 选课服务，请稍后重试。"

    @filter.command("博雅")
    async def boya(self, event: AstrMessageEvent, action: str = "状态"):
        if not self.allowed(event):
            return
        operation = COMMANDS.get(action)
        if operation is None:
            yield event.plain_result("可用指令：/博雅 状态、/博雅 已选、/博雅 统计")
            return
        yield event.plain_result(await self.query(operation))

    @filter.on_llm_request()
    async def limit_tool(self, event: AstrMessageEvent, req: ProviderRequest):
        if req.func_tool is not None and not self.allowed(event):
            req.func_tool.remove_tool(TOOL)

    @filter.llm_tool(name=TOOL)
    async def query_buaa_boya(self, event: AstrMessageEvent, action: str):
        '''查询北航博雅选课服务。例如“博雅还差几门”查统计，“我选了哪些博雅课”查已选，“选课服务还在跑吗”查状态。

        Args:
            action(string): 必须是“状态”、“已选”或“统计”。
        '''
        if not self.allowed(event):
            return "此查询仅在已授权的 QQ 私聊可用。"
        operation = COMMANDS.get(action)
        if operation is None:
            return "请选择状态、已选或统计。"
        # Send the actual school data to the user without including it in the
        # model's tool result or asking a model to interpret the numbers.
        await event.send(event.plain_result(await self.query(operation)))
        return "已把查询结果直接发给用户，无需重复回复。"

    async def deliver(self):
        while True:
            await asyncio.sleep(10)
            try:
                await self.send_pending()
            except Exception as error:
                logger.warning("BUAA QQ delivery deferred (%s)", type(error).__name__)
                await asyncio.sleep(50)

    async def send_pending(self):
        outbox = BRIDGE_DIR / "outbox"
        events = sorted(outbox.glob("*.json"))
        if not events:
            return
        platform = next(
            (
                adapter
                for adapter in self.context.platform_manager.platform_insts
                if adapter.meta().id == self.owner["platform"]
                and adapter.meta().name == "qq_official"
            ),
            None,
        )
        if platform is None:
            return
        sender = SimpleNamespace(bot=platform.client)
        for event in events[:10]:
            message = json.loads(event.read_text())
            # This is a proactive QQ message. Supplying a cached msg_id
            # would turn it into a reply and make it expire with that message.
            receipt = await asyncio.wait_for(
                QQOfficialMessageEvent.post_c2c_message(
                    sender,
                    openid=self.owner["openid"],
                    content=f'{message["title"]}\n{message["body"]}',
                ),
                timeout=20,
            )
            if not getattr(receipt, "id", None):
                raise RuntimeError("QQ did not acknowledge the message")
            event.unlink()
