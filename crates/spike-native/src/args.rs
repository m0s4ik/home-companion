// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! A tiny `--key value` / `--flag` parser, enough for two spike binaries.

use std::collections::HashMap;
use std::str::FromStr;

pub struct Args {
    values: HashMap<String, String>,
    flags: Vec<String>,
}

impl Args {
    /// Parses the process arguments. `flags` lists the options that take no value.
    pub fn parse(flags: &[&str]) -> Result<Self, String> {
        Self::from_iter(std::env::args().skip(1), flags)
    }

    pub fn from_iter(it: impl IntoIterator<Item = String>, flags: &[&str]) -> Result<Self, String> {
        let mut values = HashMap::new();
        let mut set = Vec::new();
        let mut it = it.into_iter();
        while let Some(a) = it.next() {
            let Some(key) = a.strip_prefix("--") else {
                return Err(format!("unexpected argument '{a}'"));
            };
            if flags.contains(&key) {
                set.push(key.to_string());
            } else {
                let v = it.next().ok_or_else(|| format!("--{key} needs a value"))?;
                values.insert(key.to_string(), v);
            }
        }
        Ok(Self { values, flags: set })
    }

    pub fn flag(&self, key: &str) -> bool {
        self.flags.iter().any(|f| f == key)
    }

    pub fn required(&self, key: &str) -> Result<&str, String> {
        self.values.get(key).map(String::as_str).ok_or_else(|| format!("--{key} is required"))
    }

    pub fn get<T: FromStr>(&self, key: &str) -> Result<Option<T>, String> {
        match self.values.get(key) {
            None => Ok(None),
            Some(v) => v.parse().map(Some).map_err(|_| format!("--{key}: invalid value '{v}'")),
        }
    }
}
