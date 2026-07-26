use crate::component::Component;

pub struct Spacer {
    height: usize,
}

impl Spacer {
    pub fn new(height: usize) -> Self {
        Self { height }
    }
}

impl Component for Spacer {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec![String::new(); self.height]
    }
}
