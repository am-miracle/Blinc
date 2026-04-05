//! Navigation history stack

use crate::route::{QueryParams, RouteParams};

/// A single history entry
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub path: String,
    pub params: RouteParams,
    pub query: QueryParams,
    pub title: Option<String>,
}

impl HistoryEntry {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            params: RouteParams::new(),
            query: QueryParams::new(),
            title: None,
        }
    }
}

/// Navigation history with back/forward stacks
#[derive(Clone, Debug)]
pub struct RouterHistory {
    pub back_stack: Vec<HistoryEntry>,
    pub forward_stack: Vec<HistoryEntry>,
    pub current: HistoryEntry,
    pub max_size: usize,
}

impl RouterHistory {
    pub fn new(initial_path: &str) -> Self {
        Self {
            back_stack: Vec::new(),
            forward_stack: Vec::new(),
            current: HistoryEntry::new(initial_path),
            max_size: 50,
        }
    }

    /// Push a new entry (clears forward stack)
    pub fn push(&mut self, entry: HistoryEntry) {
        self.forward_stack.clear();
        self.back_stack.push(self.current.clone());
        if self.back_stack.len() > self.max_size {
            self.back_stack.remove(0);
        }
        self.current = entry;
    }

    /// Replace current entry (no back stack change)
    pub fn replace(&mut self, entry: HistoryEntry) {
        self.current = entry;
    }

    /// Go back. Returns the new current entry, or None if can't go back.
    pub fn back(&mut self) -> Option<&HistoryEntry> {
        if let Some(prev) = self.back_stack.pop() {
            self.forward_stack.push(self.current.clone());
            self.current = prev;
            Some(&self.current)
        } else {
            None
        }
    }

    /// Go forward. Returns the new current entry, or None if can't go forward.
    pub fn forward(&mut self) -> Option<&HistoryEntry> {
        if let Some(next) = self.forward_stack.pop() {
            self.back_stack.push(self.current.clone());
            self.current = next;
            Some(&self.current)
        } else {
            None
        }
    }

    pub fn can_go_back(&self) -> bool {
        !self.back_stack.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward_stack.is_empty()
    }
}
