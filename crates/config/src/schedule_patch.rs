//! Field patch for one `[[schedules]]` row.
//!
//! An update names the keys it changes. Every other key, including unknown keys
//! and their comments, stays in the file. `job`, `habit_text`, and `deliver`
//! are removed only when the caller lists them in `remove`.

use std::collections::{BTreeMap, BTreeSet};

use toml_edit::{Item, Table, Value};

/// One named change. Absence of a key means "leave the file alone".
#[derive(Debug, Clone)]
pub(crate) enum FieldEdit {
    /// Write this value over the key.
    Set(Value),
    /// Delete the key. Protected fields also need [`SchedulePatch::remove`].
    Clear,
}

/// Keys to change on an existing `[[schedules]]` row.
#[derive(Debug, Clone, Default)]
pub struct SchedulePatch {
    edits: BTreeMap<String, FieldEdit>,
    /// Explicit deletions. Required for `job`, `habit_text`, and `deliver`.
    pub remove: BTreeSet<String>,
}

const PROTECTED: &[&str] = &["job", "habit_text", "deliver"];

impl SchedulePatch {
    /// Build a patch from an editor tool object.
    ///
    /// `name` is the lookup key and is not an edit. `new_name` renames the row.
    /// JSON `null` clears a key. Clearing `job`, `habit_text`, or `deliver`
    /// also requires that name in `remove`.
    pub fn from_tool_args(args: &serde_json::Value) -> Result<Self, String> {
        let obj = args
            .as_object()
            .ok_or_else(|| "schedule patch must be a JSON object".to_string())?;
        let mut patch = Self::default();
        if let Some(remove) = obj.get("remove") {
            patch.remove = parse_remove(remove)?;
        }
        for (key, value) in obj {
            if key == "name" || key == "remove" {
                continue;
            }
            if key == "new_name" {
                insert_new_name(&mut patch, value)?;
                continue;
            }
            insert_json(&mut patch, key, value)?;
        }
        reject_conflict(&patch)?;
        reject_protected(&patch)?;
        Ok(patch)
    }

    #[cfg(test)]
    pub(crate) fn set_string(&mut self, key: impl Into<String>, value: &str) {
        self.edits
            .insert(key.into(), FieldEdit::Set(Value::from(value.to_string())));
    }

    #[cfg(test)]
    pub(crate) fn set_bool(&mut self, key: impl Into<String>, value: bool) {
        self.edits
            .insert(key.into(), FieldEdit::Set(Value::from(value)));
    }

    #[cfg(test)]
    pub(crate) fn set_i64(&mut self, key: impl Into<String>, value: i64) {
        self.edits
            .insert(key.into(), FieldEdit::Set(Value::from(value)));
    }

    #[cfg(test)]
    pub(crate) fn clear_key(&mut self, key: impl Into<String>) {
        self.edits.insert(key.into(), FieldEdit::Clear);
    }

    /// True when this patch names `key` as a set, a clear, or a removal.
    pub fn touches(&self, key: &str) -> bool {
        self.remove.contains(key) || self.edits.contains_key(key)
    }

    /// Name written into the row. `lookup` stays when the patch does not rename.
    pub(crate) fn resulting_name<'a>(&'a self, lookup: &'a str) -> &'a str {
        match self.edits.get("name") {
            Some(FieldEdit::Set(value)) => value
                .as_str()
                .filter(|text| !text.trim().is_empty())
                .unwrap_or(lookup),
            _ => lookup,
        }
    }
}

/// Apply `patch` to an existing row. Other keys and their comments stay.
pub(crate) fn apply(table: &mut Table, patch: &SchedulePatch) -> Result<(), String> {
    reject_conflict(patch)?;
    reject_protected(patch)?;
    reject_empty_goal(table, patch)?;
    for key in &patch.remove {
        table.remove(key);
    }
    for (key, edit) in &patch.edits {
        if patch.remove.contains(key) {
            continue;
        }
        match edit {
            FieldEdit::Clear => {
                table.remove(key);
            }
            FieldEdit::Set(value) => set_value(table, key, value.clone()),
        }
    }
    Ok(())
}

fn parse_remove(value: &serde_json::Value) -> Result<BTreeSet<String>, String> {
    let list = value
        .as_array()
        .ok_or_else(|| "remove must be an array of field names".to_string())?;
    let mut names = BTreeSet::new();
    for item in list {
        let name = item
            .as_str()
            .ok_or_else(|| "remove entries must be strings".to_string())?
            .trim();
        if name.is_empty() {
            return Err("remove entries must be non-empty".into());
        }
        names.insert(name.to_string());
    }
    Ok(names)
}

fn insert_new_name(patch: &mut SchedulePatch, value: &serde_json::Value) -> Result<(), String> {
    let name = value
        .as_str()
        .ok_or_else(|| "new_name must be a string".to_string())?
        .trim();
    if name.is_empty() {
        return Err("new_name is empty".into());
    }
    patch.edits.insert(
        "name".to_string(),
        FieldEdit::Set(Value::from(name.to_string())),
    );
    Ok(())
}

fn insert_json(
    patch: &mut SchedulePatch,
    key: &str,
    value: &serde_json::Value,
) -> Result<(), String> {
    if value.is_null() {
        patch.edits.insert(key.to_string(), FieldEdit::Clear);
        return Ok(());
    }
    patch
        .edits
        .insert(key.to_string(), FieldEdit::Set(json_to_toml(value)?));
    Ok(())
}

fn json_to_toml(value: &serde_json::Value) -> Result<Value, String> {
    match value {
        serde_json::Value::Bool(flag) => Ok(Value::from(*flag)),
        serde_json::Value::String(text) => Ok(Value::from(text.clone())),
        serde_json::Value::Number(number) => json_number(number),
        serde_json::Value::Array(items) => json_string_array(items),
        _ => {
            Err("schedule patch values must be strings, numbers, booleans, or string arrays".into())
        }
    }
}

fn json_string_array(items: &[serde_json::Value]) -> Result<Value, String> {
    let mut array = toml_edit::Array::new();
    for item in items {
        let text = item
            .as_str()
            .ok_or_else(|| "schedule patch arrays must contain strings".to_string())?;
        array.push(text);
    }
    Ok(Value::Array(array))
}

fn json_number(number: &serde_json::Number) -> Result<Value, String> {
    match number.as_i64() {
        Some(value) => Ok(Value::from(value)),
        None => Err("schedule patch numbers must be integers".into()),
    }
}

fn reject_conflict(patch: &SchedulePatch) -> Result<(), String> {
    for key in &patch.remove {
        if let Some(FieldEdit::Set(_)) = patch.edits.get(key) {
            return Err(format!("patch sets {key} and also removes it"));
        }
    }
    Ok(())
}

fn reject_protected(patch: &SchedulePatch) -> Result<(), String> {
    for key in PROTECTED {
        if clears(patch, key) && !patch.remove.contains(*key) {
            return Err(format!("refusing to remove {key}; name it in remove"));
        }
    }
    Ok(())
}

fn clears(patch: &SchedulePatch, key: &str) -> bool {
    match patch.edits.get(key) {
        Some(FieldEdit::Clear) => true,
        Some(FieldEdit::Set(value)) => blank_string(value),
        None => false,
    }
}

fn blank_string(value: &Value) -> bool {
    value.as_str().is_some_and(|text| text.trim().is_empty())
}

fn reject_empty_goal(table: &Table, patch: &SchedulePatch) -> Result<(), String> {
    if goal_becomes_empty(patch) && job_remains(table, patch) {
        return Err("refusing goal = \"\" on a job entry".into());
    }
    Ok(())
}

fn goal_becomes_empty(patch: &SchedulePatch) -> bool {
    if patch.remove.contains("goal") {
        return true;
    }
    match patch.edits.get("goal") {
        Some(FieldEdit::Clear) => true,
        Some(FieldEdit::Set(value)) => blank_string(value),
        None => false,
    }
}

fn job_remains(table: &Table, patch: &SchedulePatch) -> bool {
    if patch.remove.contains("job") {
        return false;
    }
    match patch.edits.get("job") {
        Some(FieldEdit::Clear) => false,
        Some(FieldEdit::Set(value)) => value.as_str().is_some_and(|text| !text.trim().is_empty()),
        None => table
            .get("job")
            .and_then(Item::as_str)
            .is_some_and(|text| !text.trim().is_empty()),
    }
}

fn set_value(table: &mut Table, key: &str, mut value: Value) {
    if let Some(Item::Value(existing)) = table.get(key) {
        *value.decor_mut() = existing.decor().clone();
    }
    table.insert(key, Item::Value(value));
}

#[cfg(test)]
#[path = "schedule_patch_tests.rs"]
mod tests;
