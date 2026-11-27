"""Private BUAA commands and proactive QQ delivery for AstrBot 4.29.4."""

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
from astrbot.core.star.filter.command import GreedyStr


BRIDGE_DIR = Path("/AstrBot/data/.buaa-bridge")
BOYA_QUERIES = {
    "状态": "status", "已选": "selected", "统计": "statistics",
    "可选": "courses", "全部": "all_courses",
}
CAMPUS_QUERIES = {
    "课表": "class", "考试": "exam", "待评教": "tes", "评教列表": "tes_all",
    "问卷": "tes_form", "服务": "status", "通知": "notifications",
}
BOYA_ACTIONS = {
    "选课": "boya_select", "退课": "boya_drop", "签到": "boya_sign",
    "自动选课": "auto_select", "自动签到": "auto_sign",
}
CAMPUS_ACTIONS = {
    "签到": "class_sign", "今日自动签到": "class_auto", "交评教": "tes_submit",
    "自动评教": "tes_auto", "校园网登录": "wifi_on", "校园网注销": "wifi_off",
    "启动服务": "server_start", "停止服务": "server_stop",
    "开启Bark": "bark_on", "关闭Bark": "bark_off", "测试通知": "test_notification",
}
TOOL = "query_buaa_boya"
CAMPUS_TOOL = "query_buaa_campus"

BOYA_HELP = """博雅查询
/博雅 状态、已选、统计、可选 [页码]、全部 [页码]
需要确认的操作
/博雅 选课|退课|签到 课程ID
/博雅 自动选课 德育=3,美育=1
/博雅 自动签到 开|关
/北航 确认 确认码"""

CAMPUS_HELP = """北航查询
/北航 课表 [今天|明天|YYYYMMDD|课程ID|学期ID]
/北航 考试、待评教、评教列表、问卷 序号、服务、通知
需要确认的操作
/北航 签到 排课ID|YYYYMMDD、今日自动签到
/北航 交评教 序号 {"answers":[1,2,null],"reason":"满分或不及格时填理由"}
/北航 自动评教、校园网登录、校园网注销
/北航 启动服务、停止服务、开启Bark、关闭Bark、测试通知 [QQ|Bark]
/北航 确认 确认码
SSO 登录和 Bark Key 配置请在 WSL 终端操作。"""


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

    async def query(self, operation: str, argument: str = "") -> str:
        async def read_result():
            reader, writer = await asyncio.open_unix_connection(
                BRIDGE_DIR / "query.sock"
            )
            try:
                request = json.dumps(
                    {"operation": operation, "argument": argument}, ensure_ascii=False
                )
                writer.write((request + "\n").encode())
                await writer.drain()
                return json.loads(await reader.readline())["text"]
            finally:
                writer.close()
                await writer.wait_closed()

        try:
            limit = 100 if operation == "confirm" else 25
            return await asyncio.wait_for(read_result(), timeout=limit)
        except (OSError, TimeoutError, ValueError, KeyError):
            return "暂时无法查询 Win 选课服务，请稍后重试。"

    @filter.command("博雅")
    async def boya(self, event: AstrMessageEvent, text: GreedyStr):
        if not self.allowed(event):
            return
        reply = await self.command(text or "状态", BOYA_QUERIES, BOYA_ACTIONS, BOYA_HELP)
        for part in message_parts(reply):
            yield event.plain_result(part)

    @filter.command("北航")
    async def buaa(self, event: AstrMessageEvent, text: GreedyStr):
        if not self.allowed(event):
            return
        # AstrBot's argument parser normalizes whitespace. Preserve JSON
        # answers from the original command text.
        original = event.get_message_str().strip().split(maxsplit=1)
        if len(original) == 2:
            text = original[1]
        reply = await self.command(text, CAMPUS_QUERIES, CAMPUS_ACTIONS, CAMPUS_HELP)
        for part in message_parts(reply):
            yield event.plain_result(part)

    async def command(self, text, queries, actions, help_text) -> str:
        parts = text.strip().split(maxsplit=1)
        action = parts[0] if parts else ""
        argument = parts[1] if len(parts) == 2 else ""
        if action == "确认":
            return await self.query("confirm", argument)
        if action in queries:
            return await self.query(queries[action], argument)
        if action in actions:
            return await self.query("prepare." + actions[action], argument)
        return help_text

    @filter.on_llm_request()
    async def limit_tool(self, event: AstrMessageEvent, req: ProviderRequest):
        if req.func_tool is not None and not self.allowed(event):
            req.func_tool.remove_tool(TOOL)
            req.func_tool.remove_tool(CAMPUS_TOOL)

    @filter.llm_tool(name=TOOL)
    async def query_buaa_boya(self, event: AstrMessageEvent, action: str, argument: str = ""):
        '''只读查询北航博雅。“博雅还差几门”查统计，“我选了哪些博雅课”查已选，“有哪些课程可选”查可选。

        Args:
            action(string): 必须是“状态”、“已选”、“统计”、“可选”或“全部”。
            argument(string): 查询可选或全部时可传页码，默认第一页。其他查询留空。
        '''
        return await self.tool_query(event, BOYA_QUERIES, action, argument)

    @filter.llm_tool(name=CAMPUS_TOOL)
    async def query_buaa_campus(self, event: AstrMessageEvent, action: str, argument: str = ""):
        '''只读查询北航课表、考试、评教、服务和通知。“明天我有什么课”用课表加明天，“考试在哪考”用考试。

        Args:
            action(string): 课表、考试、待评教、评教列表、问卷、服务或通知。
            argument(string): 课表可填今天、明天、YYYYMMDD、课程 ID 或学期 ID。问卷填评教序号。其他查询留空。
        '''
        return await self.tool_query(event, CAMPUS_QUERIES, action, argument)

    async def tool_query(self, event, queries, action, argument):
        if not self.allowed(event):
            return "此查询仅在已授权的 QQ 私聊可用。"
        operation = queries.get(action)
        if operation is None:
            return "请选择支持的只读查询。需要修改时请在私聊输入固定指令。"
        # Send the actual school data to the user without including it in the
        # model's tool result or asking a model to interpret the numbers.
        for part in message_parts(await self.query(operation, argument)):
            await event.send(event.plain_result(part))
        return "已把查询结果直接发给用户，无需重复回复。"

    async def deliver(self):
        while True:
            await asyncio.sleep(10)
            try:
                await self.send_pending()
            except Exception as error:
                logger.warning(f"BUAA QQ delivery deferred ({type(error).__name__})")
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
            logger.info("BUAA QQ notification accepted")


def message_parts(text):
    # Keep each reply comfortably below QQ's message size limit.
    while len(text) > 900:
        end = text.rfind("\n", 0, 900)
        if end <= 0:
            end = 900
        yield text[:end]
        text = text[end:].lstrip("\n")
    if text:
        yield text
