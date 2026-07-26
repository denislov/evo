use tui::api::component::Text;
use tui::api::render::Tui;
use tui::api::terminal::ProcessTerminal;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let terminal = ProcessTerminal::new();
    let mut tui = Tui::new(terminal);
    tui.add_child(Box::new(Text::new("tui Rust renderer PoC")));
    tui.render_once()?;
    Ok(())
}
