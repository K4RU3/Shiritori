use bincode::{Decode, Encode};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc};
use tokio::sync::RwLock;
use thiserror::Error;

use crate::arc_rwlock;

#[derive(Debug, Encode, Decode)]
struct Node {
    word: String,
    children: HashMap<u32, Node>,
}

impl Node {
    fn new(word: String) -> Self {
        Self {
            word,
            children: HashMap::new(),
        }
    }

    fn insert(&mut self, word: String) {
        let dist = levenshtein(&word, &self.word);
        if let Some(child) = self.children.get_mut(&(dist as u32)) {
            child.insert(word);
        } else {
            self.children.insert(dist as u32, Node::new(word));
        }
    }

    fn search_fuzzy(&self, query: &str, max_dist: usize, results: &mut Vec<String>) {
        let dist = levenshtein(query, &self.word);
        if dist <= max_dist {
            results.push(self.word.clone());
        }

        let min = dist.saturating_sub(max_dist) as u32;
        let max = dist.saturating_add(max_dist) as u32;

        for (d, child) in &self.children {
            if *d >= min && *d <= max {
                child.search_fuzzy(query, max_dist, results);
            }
        }
    }

    fn search_match(&self, query: &str, mode: MatchMode, results: &mut Vec<String>) {
        let matched = match mode {
            MatchMode::Prefix => self.word.starts_with(query),
            MatchMode::Suffix => self.word.ends_with(query),
            MatchMode::Substring => self.word.contains(query),
            MatchMode::Exact => self.word == query,
        };

        if matched {
            results.push(self.word.clone());
        }

        for child in self.children.values() {
            child.search_match(query, mode, results);
        }
    }
}

#[derive(Debug, Encode, Decode)]
pub struct FuzzyIndex {
    root: Option<Node>,
}

#[derive(Debug, Clone, Copy)]
pub enum MatchMode {
    Prefix,
    Suffix,
    Substring,
    Exact,
}

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Bincode encode error: {0}")]
    Encode(#[from] bincode::error::EncodeError),

    #[error("Bincode decode error: {0}")]
    Decode(#[from] bincode::error::DecodeError),
}

impl FuzzyIndex {
    pub fn new() -> Self {
        Self { root: None }
    }

    /// 単語を追加
    pub fn add_word<S: Into<String>>(&mut self, word: S) {
        let word = word.into();
        if let Some(root) = &mut self.root {
            root.insert(word);
        } else {
            self.root = Some(Node::new(word));
        }
    }

    /// 単語群を追加
    pub fn add_words<I, S>(&mut self, words: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for word in words {
            self.add_word(word);
        }
    }

    /// 曖昧検索（レーベンシュタイン距離）
    pub fn search_fuzzy(&self, query: &str, max_dist: usize) -> Vec<String> {
        let mut results = Vec::new();
        if let Some(root) = &self.root {
            root.search_fuzzy(query, max_dist, &mut results);
        }
        results
    }

    /// 部分一致検索（モード指定）
    pub fn search_match(&self, query: &str, mode: MatchMode) -> Vec<String> {
        let mut results = Vec::new();
        if let Some(root) = &self.root {
            root.search_match(query, mode, &mut results);
        }
        results
    }

    /// 保存
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), IndexError> {
        let config = bincode::config::standard();
        let encoded: Vec<u8> = bincode::encode_to_vec(self, config)?;
        fs::write(path, encoded)?;
        Ok(())
    }

    /// 読み込み
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, IndexError> {
        let config = bincode::config::standard();
        let data = fs::read(path)?;
        let (tree, _len): (FuzzyIndex, usize) = bincode::decode_from_slice(&data, config)?;
        Ok(tree)
    }
}

#[derive(Debug, Clone)]
pub struct SharedFuzzyIndex {
    inner: Arc<RwLock<FuzzyIndex>>,
}

impl SharedFuzzyIndex {
    pub fn new() -> Self {
        Self {
            inner: arc_rwlock!(FuzzyIndex::new()),
        }
    }

    /// 単語追加
    pub async fn add_word(&self, word: String) {
        let mut index = self.inner.write().await;
        index.add_word(word);
    }

    // 単語群追加
    pub async fn add_words<I, S>(&self, words: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut idx = self.inner.write().await;
        idx.add_words(words);
    }

    /// 曖昧検索
    pub async fn search_fuzzy(&self, query: &str, max_dist: usize) -> Vec<String> {
        let index = self.inner.read().await;
        index.search_fuzzy(query, max_dist)
    }

    /// 部分一致検索
    pub async fn search_match(&self, query: &str, mode: MatchMode) -> Vec<String> {
        let index = self.inner.read().await;
        index.search_match(query, mode)
    }

    /// 保存
    pub async fn save<P: AsRef<std::path::Path>>(&self, path: P) -> Result<(), IndexError> {
        let index = self.inner.read().await;
        index.save(path)?;
        Ok(())
    }

    /// 読み込み
    pub fn load<P: AsRef<std::path::Path>>(path: P) -> Result<Self, IndexError> {
        let index = FuzzyIndex::load(path)?;
        Ok(Self {
            inner: arc_rwlock!(index),
        })
    }
}

/// レーベンシュタイン距離
pub fn levenshtein(a: &str, b: &str) -> usize {
    let mut costs: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut last_cost = i;
        costs[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let new_cost = if ca == cb {
                last_cost
            } else {
                1 + last_cost.min(costs[j]).min(costs[j + 1])
            };
            last_cost = costs[j + 1];
            costs[j + 1] = new_cost;
        }
    }
    costs[b.len()]
}
