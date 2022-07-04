use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    fmt::{Debug, Display},
    mem::take,
    ops::Deref,
};

use turbo_tasks::TaskId;

const MAX_SCOPES: u8 = 10;

#[derive(Hash, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskScopeId {
    id: usize,
}

impl Display for TaskScopeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TaskScopeId {}", self.id)
    }
}

impl Debug for TaskScopeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TaskScopeId {}", self.id)
    }
}

impl Deref for TaskScopeId {
    type Target = usize;

    fn deref(&self) -> &Self::Target {
        &self.id
    }
}

impl From<usize> for TaskScopeId {
    fn from(id: usize) -> Self {
        Self { id }
    }
}

#[derive(Clone)]
pub struct TaskScopeList {
    pub root: Option<TaskScopeId>,
    count: u8,
    list: [(TaskScopeId, usize); MAX_SCOPES as usize],
}

pub enum SetRootResult {
    New,
    Existing,
    AlreadyOtherRoot,
}
pub enum AddResult {
    Root,
    Existing,
    New,
    Full,
}
pub enum RemoveResult {
    Root,
    NoEntry,
    Decreased,
    Removed,
}
pub enum ContainsResult {
    Root,
    Entry,
    NoEntry,
}

impl TaskScopeList {
    pub fn set_root(&mut self, id: TaskScopeId) -> SetRootResult {
        if let Some(existing) = self.root {
            if existing == id {
                SetRootResult::Existing
            } else {
                SetRootResult::AlreadyOtherRoot
            }
        } else {
            self.root = Some(id);
            SetRootResult::New
        }
    }
    pub fn try_add(&mut self, id: TaskScopeId) -> AddResult {
        if self.root == Some(id) {
            return AddResult::Root;
        }
        for i in 0..self.count {
            let item = &mut self.list[i as usize];
            if item.0 == id {
                item.1 += 1;
                return AddResult::Existing;
            }
        }
        if self.count == MAX_SCOPES {
            return AddResult::Full;
        }
        self.list[self.count as usize] = (id, 1);
        self.count += 1;
        AddResult::New
    }
    pub fn remove(&mut self, id: TaskScopeId) -> RemoveResult {
        if self.root == Some(id) {
            return RemoveResult::Root;
        }
        for i in 0..self.count {
            let (item_id, ref mut count) = self.list[i as usize];
            if item_id == id {
                *count -= 1;
                if *count == 0 {
                    if i != self.count {
                        self.list[i as usize] = self.list[self.count as usize];
                    }
                    self.count -= 1;
                    return RemoveResult::Removed;
                } else {
                    return RemoveResult::Decreased;
                }
            }
        }
        RemoveResult::NoEntry
    }
    pub fn contains(&self, id: TaskScopeId) -> ContainsResult {
        if self.root == Some(id) {
            return ContainsResult::Root;
        }
        for i in 0..self.count {
            if self.list[i as usize].0 == id {
                return ContainsResult::Entry;
            }
        }
        ContainsResult::NoEntry
    }
    pub fn iter<'a>(&'a self) -> TaskScopeListIterator<'a> {
        TaskScopeListIterator {
            root: self.root,
            i: 0,
            count: self.count,
            list: &self.list,
        }
    }
    pub fn iter_non_root<'a>(&'a self) -> TaskScopeListIterator<'a> {
        TaskScopeListIterator {
            root: None,
            i: 0,
            count: self.count,
            list: &self.list,
        }
    }
}

impl Default for TaskScopeList {
    fn default() -> Self {
        Self {
            root: None,
            count: 0,
            list: [(TaskScopeId::from(0), 0); MAX_SCOPES as usize],
        }
    }
}

pub struct TaskScopeListIterator<'a> {
    root: Option<TaskScopeId>,
    i: u8,
    count: u8,
    list: &'a [(TaskScopeId, usize); MAX_SCOPES as usize],
}

impl<'a> Iterator for TaskScopeListIterator<'a> {
    type Item = TaskScopeId;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(root) = self.root {
            self.root = None;
            return Some(root);
        }
        if self.i == self.count {
            return None;
        }
        let item = self.list[self.i as usize].0;
        self.i += 1;
        Some(item)
    }
}

pub struct TaskScope {
    /// Number of active parents or tasks. Non-zero value means the scope is
    /// active
    active: usize,
    /// When not active, this list contains all dirty tasks.
    /// When the scope becomes active, these need to be scheduled.
    pub dirty_tasks: HashSet<TaskId>,
    /// All child scopes, when the scope becomes active, child scopes need to
    /// become active too
    pub children: HashMap<TaskScopeId, usize>,
}

impl TaskScope {
    pub fn new() -> Self {
        Self {
            active: 0,
            dirty_tasks: HashSet::new(),
            children: HashMap::new(),
        }
    }

    pub fn new_active() -> Self {
        Self {
            active: 1,
            dirty_tasks: HashSet::new(),
            children: HashMap::new(),
        }
    }

    pub fn is_active(&self) -> bool {
        self.active > 0
    }
    /// increments the active counter, returns list of tasks that need to be
    /// scheduled and list of child scope that need to be incremented after
    /// releasing the scope lock
    #[must_use]
    pub fn increment_active(
        &mut self,
        more_jobs: &mut Vec<TaskScopeId>,
    ) -> Option<HashSet<TaskId>> {
        self.active += 1;
        if self.active == 1 {
            more_jobs.extend(self.children.keys().cloned());
            Some(take(&mut self.dirty_tasks))
        } else {
            None
        }
    }
    /// decrement the active counter, returns list of child scopes that need to
    /// be decremented after releasing the scope lock
    #[must_use]
    pub fn decrement_active(&mut self, more_jobs: &mut Vec<TaskScopeId>) {
        self.active -= 1;
        if self.active == 0 {
            more_jobs.extend(self.children.keys().cloned());
        }
    }

    /// Add a child scope. Returns true, when the child scope need to have it's
    /// active counter increased.
    #[must_use]
    pub fn add_child(&mut self, child: TaskScopeId) -> bool {
        match self.children.entry(child) {
            Entry::Occupied(mut e) => {
                *e.get_mut() += 1;
                false
            }
            Entry::Vacant(e) => {
                e.insert(1);
                self.active > 0
            }
        }
    }

    /// Removes a child scope. Returns true, when the child scope need to have
    /// it's active counter decreased.
    #[must_use]
    pub fn remove_child(&mut self, child: TaskScopeId) -> bool {
        match self.children.entry(child) {
            Entry::Occupied(mut e) => {
                let value = e.get_mut();
                *value -= 1;
                *value == 0 && self.active > 0
            }
            Entry::Vacant(_) => {
                panic!("A child scope was removed that was never added")
            }
        }
    }
}
