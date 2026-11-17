use tabled::{
    builder::Builder,
    settings::{Alignment, Style},
};

use std::env;
use std::error::Error;
use std::path::PathBuf;

pub fn get_path(path: &str) -> Result<PathBuf, Box<dyn Error>> {
    if let Some(dir) = env::var_os("BUAA_DATA_DIR") {
        return Ok(PathBuf::from(dir).join(path));
    }
    let file_path = env::current_exe()?
        .parent()
        .ok_or("Failed to get parent directory")?
        .to_path_buf()
        .join(path);
    Ok(file_path)
}

pub fn print_table(builder: Builder) {
    let table = builder
        .index()
        .build()
        .with(Style::modern_rounded())
        .with(Alignment::center())
        .with(Alignment::center_vertical())
        .to_string();
    println!("{table}");
}
