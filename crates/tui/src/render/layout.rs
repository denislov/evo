use crate::render::{drop_columns, truncate_to_width, visible_width};

const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Rect {
    pub const fn new(x: usize, y: usize, width: usize, height: usize) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn right(self) -> usize {
        self.x.saturating_add(self.width)
    }

    pub fn bottom(self) -> usize {
        self.y.saturating_add(self.height)
    }

    pub fn inset(self, horizontal: usize, vertical: usize) -> Self {
        let horizontal_total = horizontal.saturating_mul(2);
        let vertical_total = vertical.saturating_mul(2);
        Self {
            x: self.x.saturating_add(horizontal.min(self.width)),
            y: self.y.saturating_add(vertical.min(self.height)),
            width: self.width.saturating_sub(horizontal_total),
            height: self.height.saturating_sub(vertical_total),
        }
    }

    pub fn intersection(self, other: Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Self::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
    }

    pub const fn contains(self, point: Point) -> bool {
        point.x >= self.x
            && point.x < self.x.saturating_add(self.width)
            && point.y >= self.y
            && point.y < self.y.saturating_add(self.height)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Point {
    pub x: usize,
    pub y: usize,
}

impl Point {
    pub const fn new(x: usize, y: usize) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitRegion<T> {
    pub rect: Rect,
    pub target: T,
}

impl<T> HitRegion<T> {
    pub const fn new(rect: Rect, target: T) -> Self {
        Self { rect, target }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitMap<T> {
    regions: Vec<HitRegion<T>>,
}

impl<T> Default for HitMap<T> {
    fn default() -> Self {
        Self {
            regions: Vec::new(),
        }
    }
}

impl<T> HitMap<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.regions.clear();
    }

    pub fn push(&mut self, region: HitRegion<T>) {
        if !region.rect.is_empty() {
            self.regions.push(region);
        }
    }

    pub fn hit(&self, point: Point) -> Option<&T> {
        self.regions
            .iter()
            .rev()
            .find(|region| region.rect.contains(point))
            .map(|region| &region.target)
    }

    pub fn regions(&self) -> &[HitRegion<T>] {
        &self.regions
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Constraint {
    Length(usize),
    Percentage(u8),
    Fill(u16),
}

pub struct Layout;

impl Layout {
    pub fn split(rect: Rect, axis: Axis, constraints: &[Constraint]) -> Vec<Rect> {
        let available = match axis {
            Axis::Horizontal => rect.width,
            Axis::Vertical => rect.height,
        };
        let lengths = resolve_lengths(available, constraints);
        let mut cursor = match axis {
            Axis::Horizontal => rect.x,
            Axis::Vertical => rect.y,
        };

        lengths
            .into_iter()
            .map(|length| {
                let child = match axis {
                    Axis::Horizontal => Rect::new(cursor, rect.y, length, rect.height),
                    Axis::Vertical => Rect::new(rect.x, cursor, rect.width, length),
                };
                cursor = cursor.saturating_add(length);
                child
            })
            .collect()
    }

    pub fn horizontal(rect: Rect, constraints: &[Constraint]) -> Vec<Rect> {
        Self::split(rect, Axis::Horizontal, constraints)
    }

    pub fn vertical(rect: Rect, constraints: &[Constraint]) -> Vec<Rect> {
        Self::split(rect, Axis::Vertical, constraints)
    }
}

fn resolve_lengths(available: usize, constraints: &[Constraint]) -> Vec<usize> {
    let mut remaining = available;
    let mut lengths = vec![0; constraints.len()];
    let mut fill_weight = 0usize;

    for (index, constraint) in constraints.iter().enumerate() {
        let requested = match constraint {
            Constraint::Length(length) => *length,
            Constraint::Percentage(percent) => {
                available.saturating_mul((*percent).min(100) as usize) / 100
            }
            Constraint::Fill(weight) => {
                fill_weight = fill_weight.saturating_add(*weight as usize);
                continue;
            }
        };
        let allocated = requested.min(remaining);
        lengths[index] = allocated;
        remaining = remaining.saturating_sub(allocated);
    }

    if remaining == 0 || fill_weight == 0 {
        return lengths;
    }

    let mut fill_remaining = remaining;
    let mut weight_remaining = fill_weight;
    for (index, constraint) in constraints.iter().enumerate() {
        let Constraint::Fill(weight) = constraint else {
            continue;
        };
        let weight = *weight as usize;
        if weight == 0 {
            continue;
        }
        let allocated = if weight == weight_remaining {
            fill_remaining
        } else {
            fill_remaining.saturating_mul(weight) / weight_remaining
        };
        lengths[index] = allocated;
        fill_remaining = fill_remaining.saturating_sub(allocated);
        weight_remaining = weight_remaining.saturating_sub(weight);
    }
    lengths
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    bounds: Rect,
    lines: Vec<String>,
}

impl Frame {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            bounds: Rect::new(0, 0, width, height),
            lines: vec![String::new(); height],
        }
    }

    pub const fn bounds(&self) -> Rect {
        self.bounds
    }

    pub fn draw(&mut self, rect: Rect, lines: &[String]) {
        let rect = rect.intersection(self.bounds);
        if rect.is_empty() {
            return;
        }
        for (row, line) in lines.iter().take(rect.height).enumerate() {
            let replacement = fit_to_width(line, rect.width);
            let target = &mut self.lines[rect.y + row];
            *target = splice_by_columns(target, rect.x, rect.width, &replacement);
        }
    }

    pub fn fill(&mut self, rect: Rect, line: &str) {
        let rect = rect.intersection(self.bounds);
        if rect.is_empty() {
            return;
        }
        let fitted = fit_to_width(line, rect.width);
        for row in rect.y..rect.bottom() {
            let target = &mut self.lines[row];
            *target = splice_by_columns(target, rect.x, rect.width, &fitted);
        }
    }

    pub fn into_lines(self) -> Vec<String> {
        self.lines
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusRing<T> {
    items: Vec<T>,
    current: Option<usize>,
}

impl<T: Copy + Eq> Default for FocusRing<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            current: None,
        }
    }
}

impl<T: Copy + Eq> FocusRing<T> {
    pub fn new(items: impl IntoIterator<Item = T>) -> Self {
        let mut ring = Self::default();
        ring.set_items(items);
        ring
    }

    pub fn set_items(&mut self, items: impl IntoIterator<Item = T>) {
        let selected = self.current();
        self.items = items.into_iter().collect();
        self.current = selected
            .and_then(|selected| self.items.iter().position(|item| *item == selected))
            .or_else(|| (!self.items.is_empty()).then_some(0));
    }

    pub fn current(&self) -> Option<T> {
        self.current
            .and_then(|index| self.items.get(index))
            .copied()
    }

    pub fn focus(&mut self, item: T) -> bool {
        let Some(index) = self.items.iter().position(|candidate| *candidate == item) else {
            return false;
        };
        self.current = Some(index);
        true
    }

    pub fn focus_next(&mut self) -> Option<T> {
        if self.items.is_empty() {
            self.current = None;
            return None;
        }
        self.current = Some(
            self.current
                .map_or(0, |index| (index + 1) % self.items.len()),
        );
        self.current()
    }

    pub fn focus_previous(&mut self) -> Option<T> {
        if self.items.is_empty() {
            self.current = None;
            return None;
        }
        self.current = Some(self.current.map_or(0, |index| {
            index.checked_sub(1).unwrap_or(self.items.len() - 1)
        }));
        self.current()
    }
}

fn fit_to_width(line: &str, width: usize) -> String {
    let mut fitted = truncate_to_width(line, width);
    let visible = visible_width(&fitted);
    if visible < width {
        fitted.push_str(&" ".repeat(width - visible));
    }
    fitted
}

fn splice_by_columns(base: &str, col: usize, width: usize, replacement: &str) -> String {
    let mut prefix = truncate_to_width(base, col);
    let prefix_width = visible_width(&prefix);
    if prefix_width < col {
        prefix.push_str(&" ".repeat(col - prefix_width));
    }
    let suffix = drop_columns(base, col.saturating_add(width));
    format!("{prefix}{SEGMENT_RESET}{replacement}{SEGMENT_RESET}{suffix}")
}
