mod auth;
mod boya;
mod boya_auto;
mod captcha;
mod class;
mod command;
mod exam;
mod notify;
#[cfg(unix)]
mod qq_bridge;
mod server;
mod tes;
mod utils;
mod wifi;

use clap::Parser;
use command::{
    Boya, BoyaAutoSub, BoyaSub, Class, ClassSub, Cli, Commands, ServerSub, Tes, TesSub, Wifi,
    WifiSub,
};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let path = utils::get_path("./").unwrap();
    if let Some(result) = background_command(&cli, &path).await {
        if let Err(error) = result {
            let area = if matches!(cli.command, Commands::Notify(_)) {
                "Notify"
            } else {
                "Server"
            };
            eprintln!("[Error]::<{area}>: {error}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = std::fs::create_dir_all(&path) {
        eprintln!("[Error]: Failed to create data directory: {error}");
        std::process::exit(1);
    }
    let session = match auth::Session::load(&path, cli.disable_tls, true) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("[Error]: Failed to load auth: {error}");
            std::process::exit(1);
        }
    };
    let context = &session.context;
    let explicit_login = matches!(&cli.command, Commands::Login { .. });

    match cli.command {
        Commands::Login { username, password } => {
            if let Some(un) = username {
                context.set_username(&un);
            };
            if let Some(pw) = password {
                context.set_password(&pw);
            }
            match context.sso().login().await {
                Ok(_) => println!("[Info]::<SSO>: Login successfully"),
                Err(e) => eprintln!("[Info]::<SSO>: Login failed: {e}"),
            };
        }
        Commands::Boya(Boya { command }) => match command {
            BoyaSub::Query { all, page } => {
                boya::query(context, all, page).await;
            }
            BoyaSub::Select { id } => {
                boya::select(context, id).await;
            }
            BoyaSub::Drop { id } => {
                boya::drop(context, id).await;
            }
            BoyaSub::Check { id } => {
                boya::check(context, id).await;
            }
            BoyaSub::Selected => {
                boya::selected(context).await;
            }
            BoyaSub::Status => {
                boya::status(context).await;
            }
            BoyaSub::Auto(_) => unreachable!("Handled before loading auth"),
        },
        Commands::Class(Class { command }) => match command {
            ClassSub::Auto => {
                class::auto(context).await;
            }
            ClassSub::Query { id } => {
                class::query(context, id).await;
            }
            ClassSub::Checkin { id } => {
                class::checkin(context, &id).await;
            }
        },
        Commands::Exam => {
            exam::exam(context).await;
        }
        Commands::Tes(Tes { command }) => match command {
            TesSub::Auto => {
                tes::auto(context).await;
            }
            TesSub::List { all } => {
                tes::list(context, all).await;
            }
        },
        Commands::Wifi(Wifi { command }) => match command {
            WifiSub::Login => {
                wifi::login(context).await;
            }
            WifiSub::Logout => {
                wifi::logout(context).await;
            }
        },
        Commands::Server(_) | Commands::Notify(_) => unreachable!("Handled before loading auth"),
    }
    match session.save(explicit_login) {
        Ok(_) => {}
        Err(_) => {
            eprintln!("[Error]: Failed to save auth data");
        }
    };
}

async fn background_command(cli: &Cli, path: &std::path::Path) -> Option<std::io::Result<()>> {
    use server::protocol::Request;
    let request = match &cli.command {
        Commands::Notify(args) => return Some(notify::command(path, &args.command).await),
        Commands::Server(server) => match server.command {
            ServerSub::Start => return Some(server::start(path, cli.disable_tls).await),
            ServerSub::Run => {
                #[cfg(unix)]
                let _qq_bridge = qq_bridge::start(path, cli.disable_tls).unwrap_or_else(|_| {
                    eprintln!("[Warning]::<QQ>: Could not start the local query bridge");
                    None
                });
                return Some(
                    server::run(
                        path,
                        boya_auto::BoyaHandler {
                            data_dir: path.to_path_buf(),
                            disable_tls: cli.disable_tls,
                        },
                    )
                    .await,
                );
            }
            ServerSub::Status => Request::Status,
            ServerSub::Off => Request::Stop,
        },
        Commands::Boya(Boya {
            command: BoyaSub::Auto(auto),
        }) => match &auto.command {
            BoyaAutoSub::Select(targets) => {
                let payload: serde_json::Map<String, serde_json::Value> = [
                    ("arts", targets.arts),
                    ("ethics", targets.ethics),
                    ("labor", targets.labor),
                    ("safety", targets.safety),
                    ("other", targets.other),
                ]
                .into_iter()
                .filter_map(|(name, count)| count.map(|n| (name.into(), n.into())))
                .collect();
                Request::Configure {
                    namespace: "boya.auto.select".into(),
                    payload: payload.into(),
                }
            }
            BoyaAutoSub::Sign { id: None } => Request::Configure {
                namespace: "boya.auto.sign".into(),
                payload: serde_json::json!({}),
            },
            BoyaAutoSub::Sign { id: Some(id) } => Request::Submit {
                kind: "boya.sign.plan".into(),
                payload: serde_json::json!({"id": id}),
            },
        },
        _ => return None,
    };
    Some(server::client(path, request).await)
}
