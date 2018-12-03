use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::path::PathBuf;
use crate::parser;
use crate::fuzzy::FuzzyVec;

/// Completions generated by completion functions.
pub struct Completions {
    /// Candidate completion entries.
    entries: Vec<Arc<String>>,
    // The currently selected entry.
    selected_index: usize,
    // The number of completion lines in the prompt.
    display_lines: usize,
    // The beginning of entries to be displayed.
    display_index: usize,
}

impl Completions {
    pub fn new(entries: Vec<Arc<String>>) -> Completions {
        const COMPLETION_LINES: usize = 5;

        Completions {
            entries,
            selected_index: 0,
            display_lines: COMPLETION_LINES,
            display_index: 0,
        }
    }

    /// Move to the next/previous entry.
    pub fn move_cursor(&mut self, offset: isize) {
        // FIXME: I think there's more sane way to handle a overflow.`
        let mut old_selected_index = self.selected_index as isize;
        old_selected_index += offset;

        let entries_len = self.len() as isize;
        if entries_len > 0 && old_selected_index > entries_len - 1 {
            old_selected_index = entries_len - 1;
        }

        if old_selected_index < 0 {
            old_selected_index = 0;
        }

        self.selected_index = old_selected_index as usize;

        if self.selected_index >= self.display_index + self.display_lines {
            self.display_index = self.selected_index - self.display_lines + 1;
        }

        if self.selected_index < self.display_index {
            self.display_index = self.selected_index;
        }

        trace!(
            "move_cursor: offset={}, index={}",
            offset,
            self.selected_index
        );
    }

    #[inline(always)]
    pub fn entries(&self) -> Vec<Arc<String>> {
        self.entries.clone()
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline(always)]
    pub fn selected(&self) -> Option<Arc<String>> {
        self.entries.get(self.selected_index).cloned()
    }

    #[inline(always)]
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    #[inline(always)]
    pub fn display_lines(&self) -> usize {
        self.display_lines
    }

    #[inline(always)]
    pub fn display_index(&self) -> usize {
        self.display_index
    }
}

#[derive(Default, Debug)]
pub struct CompletionContext {
    pub words: Vec<Arc<String>>,
    /// The index of the current word in `words`.
    pub current_word_index: isize,
    /// The whole line.
    pub line: String,
    /// The offset of the beginning of the current word in `line`.
    pub current_word_offset: usize,
    pub current_word_len: usize,
    pub user_cursor: usize,
}

impl CompletionContext {
    pub fn current_word(&self) -> Option<Arc<String>> {
        self.words
            .get(self.current_word_index as usize)
            .cloned()
    }
}

/// The `Completion` Builder.
pub struct CompGen {
    entries: Vec<Arc<String>>,
    query: Option<Arc<String>>,
}

impl CompGen {
    #[inline]
    pub fn new() -> CompGen {
        CompGen {
            entries: Vec::new(),
            query: None,
        }
    }

    #[inline]
    pub fn entries(mut self, entries: Vec<Arc<String>>) -> CompGen {
        self.entries = entries;
        self
    }

    #[inline]
    pub fn search(mut self, query: Option<Arc<String>>) -> CompGen {
        self.query = query;
        self
    }

    #[inline]
    pub fn build(self) -> Completions {
        let results = match self.query {
            Some(query) => FuzzyVec::from_vec(self.entries).search(&query),
            None => self.entries,
        };

        Completions::new(results)
    }
}

fn path_completion(ctx: &CompletionContext) -> Completions {
    let given_dir = ctx.current_word().map(|s| (&*s).clone());
    trace!("path_completion: current='{:?}', dir='{:?}'", ctx.current_word(), given_dir);
    let dirent = match &given_dir {
        Some(given_dir) if given_dir.ends_with('/') => {
            std::fs::read_dir(given_dir)
        },
        Some(given_dir) if given_dir.contains('/') => {
            // Remove the last part: `/Users/chandler/Docum' -> `/users/chandler'
            std::fs::read_dir(PathBuf::from(given_dir.clone()).parent().unwrap())
        },
        _ => {
            std::fs::read_dir(".")
        }
    };

    let mut entries = Vec::new();
    if let Ok(dirent) = dirent {
        for entry in dirent {
            entries.push(Arc::new(
                entry
                    .unwrap()
                    .path()
                    .to_str()
                    .unwrap()
                    .to_owned()
            ));
        }
    }

    CompGen::new()
        .entries(entries)
        .search(ctx.current_word())
        .build()
}

fn cmd_completion(ctx: &CompletionContext) -> Completions {
    match ctx.current_word() {
        Some(query) => crate::path::complete(&query),
        None => crate::path::complete(""),
    }
}

type CompletionFunc = fn(&CompletionContext) -> Completions;
lazy_static! {
    static ref COMPLETION_FUNCS: Mutex<BTreeMap<String, CompletionFunc>> =
        { Mutex::new(BTreeMap::new()) };
}

pub fn call_completion(ctx: &CompletionContext) -> Completions {
    if ctx.current_word_index == 0 {
        // The cursor is at the first word, namely, the command.
        cmd_completion(ctx)
    } else {
        let funcs = COMPLETION_FUNCS.lock().unwrap();

        // This `ctx.words[0]` never panic: `ctx.words.len() > 0` is always true
        // since `current_word_index` is larger than 0.
        if let Some(func) = funcs.get(&*ctx.words[0]) {
            // A dedicated completion function is available.
            func(ctx)
        } else {
            // There are no completion fuctions. Use path completion
            // instead.
            path_completion(ctx)
        }
    }
}

pub fn extract_completion_context(user_input: &str, user_cursor: usize) -> CompletionContext {
    let line = user_input.to_string();

    // A Poor man's command line parser.
    // TODO: Support single-quoted string.
    // TODO: Skip envinroment variable assignments like: `RAILS_env=test rspec`
    // TODO: Use `parser::parse` instead.
    //
    // Example:
    //     echo $(read-file -avh --from-stdin --other-opt < hello.bin) | hexdump -C
    //                                 ^ cursor is here

    // Before the cursor:
    //     words = ['read-file", "-avh"]
    //     word = "--from-"
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_string = false;
    let mut prev_ch = '\x00';
    let mut current_word_offset = 0;
    for (offset, ch) in line.chars().take(user_cursor).enumerate() {
        match (in_string, prev_ch, ch) {
            (true, '\\', '"') => {
                word = word.trim_matches('\\').to_owned();
                word.push('"');
            }
            (true, _, '"') => in_string = false,
            (false, _, '"') => in_string = true,
            (false, _, ' ') => {
                words.push(Arc::new(word));
                word = String::new();
                current_word_offset = offset + 1;
            }
            (false, _, ch) if !parser::is_valid_word_char(ch) && ch != '*' && ch != '?' => {
                words = Vec::new();
                word = String::new();
                current_word_offset = offset + 1;
            }
            (_, _, ch) => word.push(ch),
        }

        prev_ch = ch;
    }

    // Case #1:
    //   $ ls foo
    //            ^ user_cursor is here (the end of line)
    let mut current_word_index = words.len() as isize;
    let mut current_word_len = 0;

    // After the cursor:
    //     words = ['read-file", "-avh", "--form-stdin"]
    //     word = ""
    for ch in line.chars().skip(user_cursor) {
        match (in_string, prev_ch, ch) {
            (true, '\\', '"') => {
                word = word.trim_matches('\\').to_owned();
                word.push('"');
            }
            (true, _, '"') => in_string = false,
            (false, _, '"') => in_string = true,
            (false, _, ch) if !parser::is_valid_word_char(ch) && ch != '*' && ch != '?' => break,
            (_, _, ch) => word.push(ch),
        }
    }

    // Case #2:
    //   $ ls foo
    //         ^ user_cursor is here (within a word)
    if !word.is_empty() {
        let word_len = word.len();
        words.push(Arc::new(word));
        current_word_index = words.len() as isize - 1;
        current_word_len = word_len;
    }

    trace!(
        "words={:?}, index={}, offset={}, len={}",
        words,
        current_word_index,
        current_word_offset,
        current_word_len
    );

    CompletionContext {
        words,
        current_word_index,
        line,
        current_word_offset,
        current_word_len,
        user_cursor,
    }
}
