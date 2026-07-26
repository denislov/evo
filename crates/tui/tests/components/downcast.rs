//! Component downcast behavior.

use tui::api::component::Component;
use tui::api::render::Tui;
use tui::api::testing::VirtualTerminal;

#[derive(Debug, Default)]
struct PlainComponent {
    renders: usize,
}

impl Component for PlainComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.renders += 1;
        vec![format!("rendered {}", self.renders)]
    }
}

#[test]
fn tui_downcasts_components_without_manual_as_any_impl() {
    let mut tui = Tui::new(VirtualTerminal::new(20, 5));
    let id = tui.add_child_with_id(Box::new(PlainComponent::default()));

    assert_eq!(tui.component_as::<PlainComponent>(id).unwrap().renders, 0);

    tui.component_as_mut::<PlainComponent>(id).unwrap().renders = 41;

    assert_eq!(tui.component_as::<PlainComponent>(id).unwrap().renders, 41);
}
