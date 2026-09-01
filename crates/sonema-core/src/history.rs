use crate::Project;

#[derive(Debug, Clone)]
struct Entry {
    label: String,
    project: Project,
}

/// Bounded project history. PCM media lives outside `Project`, so snapshots are
/// small and cloning them never copies recorded audio.
#[derive(Debug, Clone)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    transaction: Option<Entry>,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::new(100)
    }
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), transaction: None, limit: limit.max(1) }
    }

    pub fn begin(&mut self, label: impl Into<String>, project: &Project) {
        if self.transaction.is_none() {
            self.transaction = Some(Entry { label: label.into(), project: project.clone() });
        }
    }

    pub fn commit(&mut self, project: &Project) -> bool {
        let Some(entry) = self.transaction.take() else { return false };
        if entry.project == *project {
            return false;
        }
        self.undo.push(entry);
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
        true
    }

    pub fn cancel(&mut self) {
        self.transaction = None;
    }

    pub fn checkpoint(&mut self, label: impl Into<String>, before: Project, after: &Project) -> bool {
        self.transaction = Some(Entry { label: label.into(), project: before });
        self.commit(after)
    }

    pub fn undo(&mut self, project: &mut Project) -> Option<String> {
        self.transaction = None;
        let entry = self.undo.pop()?;
        let label = entry.label.clone();
        self.redo.push(Entry { label: entry.label, project: project.clone() });
        *project = entry.project;
        Some(label)
    }

    pub fn redo(&mut self, project: &mut Project) -> Option<String> {
        self.transaction = None;
        let entry = self.redo.pop()?;
        let label = entry.label.clone();
        self.undo.push(Entry { label: entry.label, project: project.clone() });
        *project = entry.project;
        Some(label)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn in_transaction(&self) -> bool {
        self.transaction.is_some()
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.transaction = None;
    }
}
