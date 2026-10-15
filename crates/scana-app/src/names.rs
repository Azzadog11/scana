//! Names the user gives to values found in the data explorer ("byte 1 of LID 01 on the body
//! computer is the headlamp switch"), kept in `$XDG_DATA_HOME/scana/names.toml`, on this
//! computer only. A missing or broken file means no names yet, never a crash.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueName {
    /// The vehicle, as Scana names it ("2024 Daihatsu Hijet Jumbo").
    pub vehicle: String,
    /// The module's address ("750/40").
    pub module: String,
    /// "DID F190" or "LID 01".
    pub id: String,
    /// The byte, counting from 0 after the echoed ID.
    pub byte: usize,
    pub name: String,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    name: Vec<ValueName>,
}

#[derive(Default)]
pub struct Names {
    list: Vec<ValueName>,
    /// `None`: don't save (screenshot mode).
    path: Option<PathBuf>,
}

impl Names {
    /// Loads the user's names. `path` is `None` to keep names in memory only.
    pub fn load(path: Option<PathBuf>) -> Self {
        let list = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| toml::from_str::<File>(&t).ok())
            .map(|f| f.name)
            .unwrap_or_default();
        Names { list, path }
    }

    pub fn get(&self, vehicle: &str, module: &str, id: &str, byte: usize) -> Option<&str> {
        self.list
            .iter()
            .find(|n| n.vehicle == vehicle && n.module == module && n.id == id && n.byte == byte)
            .map(|n| n.name.as_str())
    }

    /// Names all bytes of an ID that have names, "byte: name".
    pub fn for_id(&self, vehicle: &str, module: &str, id: &str) -> Vec<(usize, &str)> {
        let mut v: Vec<(usize, &str)> = self
            .list
            .iter()
            .filter(|n| n.vehicle == vehicle && n.module == module && n.id == id)
            .map(|n| (n.byte, n.name.as_str()))
            .collect();
        v.sort();
        v
    }

    /// Sets (or with an empty name, removes) a name, and saves. Returns an error message if
    /// the file couldn't be written.
    pub fn set(&mut self, key: ValueName) -> Result<(), String> {
        self.list
            .retain(|n| !(n.vehicle == key.vehicle && n.module == key.module && n.id == key.id && n.byte == key.byte));
        if !key.name.trim().is_empty() {
            self.list.push(ValueName { name: key.name.trim().to_string(), ..key });
        }
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else { return Ok(()) };
        let text = toml::to_string(&File { name: self.list.clone() }).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("can't make {}: {e}", dir.display()))?;
        }
        let body = format!("# Names you gave to values in Scana's data explorer.\n\n{text}");
        std::fs::write(path, body).map_err(|e| format!("can't save {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_save_and_load() {
        let dir = std::env::temp_dir().join(format!("scana-names-test-{}", std::process::id()));
        let path = dir.join("names.toml");
        let mut n = Names::load(Some(path.clone()));
        let key = |name: &str| ValueName {
            vehicle: "Demo".into(),
            module: "750/40".into(),
            id: "LID 01".into(),
            byte: 1,
            name: name.into(),
        };
        n.set(key("Headlamp switch")).unwrap();
        let again = Names::load(Some(path.clone()));
        assert_eq!(again.get("Demo", "750/40", "LID 01", 1), Some("Headlamp switch"));
        assert_eq!(again.for_id("Demo", "750/40", "LID 01"), vec![(1, "Headlamp switch")]);
        n.set(key("")).unwrap();
        assert_eq!(Names::load(Some(path)).get("Demo", "750/40", "LID 01", 1), None);
        let _ = std::fs::remove_dir_all(dir);
        // In memory only.
        let mut m = Names::load(None);
        m.set(key("x")).unwrap();
        assert_eq!(m.get("Demo", "750/40", "LID 01", 1), Some("x"));
    }
}
