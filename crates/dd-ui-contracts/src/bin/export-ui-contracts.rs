use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("provide the output TypeScript path")?;
    std::fs::write(output, dd_ui_contracts::wire_primitives_typescript())?;
    Ok(())
}
