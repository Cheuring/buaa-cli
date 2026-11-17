use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(version = env!("CARGO_PKG_VERSION"), about = "A cli for BUAA")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
    #[arg(short, long, global = true, default_value_t = false)]
    pub disable_tls: bool,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    #[command(about = "Login for SSO")]
    Login {
        #[clap(short, long)]
        username: Option<String>,
        #[clap(short, long)]
        password: Option<String>,
    },
    #[command(about = "Boya Course")]
    Boya(Boya),
    #[command(about = "Smart Classroom")]
    Class(Class),
    #[command(about = "Exam List")]
    Exam,
    #[command(about = "Teacher Evaluation system (With some Warnings)")]
    Tes(Tes),
    #[command(about = "BUAA-Wifi")]
    Wifi(Wifi),
    /// Manage the local background task server
    Server(Server),
}

#[derive(Parser, Debug)]
pub struct Boya {
    #[clap(subcommand)]
    pub command: BoyaSub,
}

#[derive(Debug, Subcommand)]
pub enum BoyaSub {
    /// Query courses and select a course by ID
    Query {
        #[arg(short, long)]
        /// By default, only optional courses are displayed
        all: bool,
        /// Page number (Default 1)
        #[arg(short, long, default_value_t = 1)]
        page: u8,
    },
    /// Selete by ID immediately. No guarantee for valid token
    Select { id: u32 },
    /// Drop by ID
    Drop { id: u32 },
    /// Check-in/out by ID, Only for courses that can check-in/out by self
    Check { id: u32 },
    /// Query selected courses
    Selected,
    /// Query statistics information
    Status,
    /// Configure resident Boya automation (requires `server start`)
    Auto(BoyaAuto),
}

#[derive(Debug, Args)]
pub struct BoyaAuto {
    #[command(subcommand)]
    pub command: BoyaAutoSub,
}

#[derive(Debug, Subcommand)]
pub enum BoyaAutoSub {
    /// Top up this semester's selected counts; 0 disables a category
    Select(SelectionTargets),
    /// Toggle continuous check-in/out, or submit one selected course by ID
    Sign { id: Option<u32> },
}

#[derive(Debug, Args)]
#[group(required = true, multiple = true)]
pub struct SelectionTargets {
    #[arg(long)]
    pub arts: Option<u32>,
    #[arg(long)]
    pub ethics: Option<u32>,
    #[arg(long)]
    pub labor: Option<u32>,
    #[arg(long)]
    pub safety: Option<u32>,
    #[arg(long)]
    pub other: Option<u32>,
}

#[derive(Debug, Args)]
pub struct Server {
    #[command(subcommand)]
    pub command: ServerSub,
}

#[derive(Debug, Subcommand)]
pub enum ServerSub {
    /// Show process status, policy settings and durable task outcomes
    Status,
    /// Start a detached background server
    Start,
    /// Stop the server, retaining pending tasks and settings
    Off,
    /// Run in the foreground (used internally by `start`)
    #[command(hide = true)]
    Run,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_arguments_and_existing_commands() {
        for args in [
            vec![
                "buaa", "boya", "auto", "select", "--arts", "5", "--labor", "3",
            ],
            vec!["buaa", "boya", "auto", "sign"],
            vec!["buaa", "boya", "auto", "sign", "123"],
            vec!["buaa", "server", "start", "--disable-tls"],
            vec!["buaa", "boya", "select", "123"],
            vec!["buaa", "class", "auto"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
        for args in [
            vec!["buaa", "boya", "auto", "select"],
            vec!["buaa", "boya", "auto", "select", "--arts", "-1"],
            vec!["buaa", "boya", "auto", "sign", "oops"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }
}

#[derive(Parser, Debug)]
pub struct Class {
    #[clap(subcommand)]
    pub command: ClassSub,
}

#[derive(Debug, Subcommand)]
pub enum ClassSub {
    /// Auto checkin for today
    Auto,
    /// Query schedules by Date, term courses by Term ID, or course schedules by Course ID
    Query {
        /// Date (format: YYYYMMDD),
        /// Term ID (e.g. `202320242` is 2024 spring term,
        /// `202420251` is 2024 autumn term
        /// Note: After 2026.09, the term ID will have 10 digits,
        /// e.g. `2026202701` is 2026 autumn term)
        /// or Course ID (from query term courses, 5 or 6 digits)
        id: String,
    },
    /// Checkin with Schedule ID or Date (format: YYYYMMDD)
    Checkin {
        /// Schedule ID or Date (format: YYYYMMDD)
        id: String,
    },
}

#[derive(Parser, Debug)]
pub struct Tes {
    #[clap(subcommand)]
    pub command: TesSub,
}

#[derive(Debug, Subcommand)]
pub enum TesSub {
    /// Auto fill all unfilled forms with 93 score
    Auto,
    /// List and fill manually
    List {
        #[arg(short, long)]
        /// By default, only unfilled forms are displayed
        all: bool,
    },
}

#[derive(Parser, Debug)]
pub struct Wifi {
    #[clap(subcommand)]
    pub command: WifiSub,
}

#[derive(Debug, Subcommand)]
pub enum WifiSub {
    Login,
    Logout,
}
