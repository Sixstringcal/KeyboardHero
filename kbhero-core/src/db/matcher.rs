use regex::Regex;
use crate::types::AppIdentity;
use super::AppId;

/// The well-known ID reserved for global (non-app-specific) shortcuts.
pub(crate) const GLOBAL_APP_ID: AppId = AppId(0);

pub(crate) struct AppMatcher {
    entries: Vec<AppMatchEntry>,
}

struct AppMatchEntry {
    app_id:        AppId,
    executables:   Vec<String>,
    title_pattern: Option<Regex>,
}

impl AppMatcher {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Register an app. Returns `Err` only if `title_pattern` is an invalid regex.
    pub fn add(
        &mut self,
        app_id:        AppId,
        executables:   Vec<String>,
        title_pattern: Option<&str>,
    ) -> Result<(), regex::Error> {
        let re = match title_pattern.filter(|p| !p.is_empty()) {
            Some(p) => Some(Regex::new(p)?),
            None => None,
        };
        self.entries.push(AppMatchEntry { app_id, executables, title_pattern: re });
        Ok(())
    }

    /// Returns the first `AppId` whose executable list (case-insensitive) and
    /// optional title pattern both match `identity`. Returns `None` when no
    /// app-specific entry matches; the caller should fall back to `GLOBAL_APP_ID`.
    pub fn resolve(&self, identity: &AppIdentity) -> Option<AppId> {
        self.entries.iter().find_map(|entry| {
            let exe_matches = entry.executables.iter()
                .any(|e| e.eq_ignore_ascii_case(&identity.executable));
            if !exe_matches {
                return None;
            }
            match &entry.title_pattern {
                Some(re) => identity.window_title.as_deref()
                    .filter(|t| re.is_match(t))
                    .map(|_| entry.app_id),
                None => Some(entry.app_id),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(exe: &str, title: Option<&str>) -> AppIdentity {
        AppIdentity {
            executable:   exe.to_string(),
            window_title: title.map(str::to_string),
            pid:          1234,
        }
    }

    #[test]
    fn resolves_exact_executable() {
        let mut m = AppMatcher::new();
        m.add(AppId(1), vec!["firefox".into()], None).unwrap();
        assert_eq!(m.resolve(&identity("firefox", None)), Some(AppId(1)));
    }

    #[test]
    fn resolves_case_insensitive() {
        let mut m = AppMatcher::new();
        m.add(AppId(1), vec!["Firefox".into()], None).unwrap();
        assert_eq!(m.resolve(&identity("firefox", None)), Some(AppId(1)));
    }

    #[test]
    fn resolves_alternate_executable_name() {
        let mut m = AppMatcher::new();
        m.add(AppId(1), vec!["firefox".into(), "firefox-esr".into()], None).unwrap();
        assert_eq!(m.resolve(&identity("firefox-esr", None)), Some(AppId(1)));
    }

    #[test]
    fn returns_none_for_unknown_app() {
        let m = AppMatcher::new();
        assert_eq!(m.resolve(&identity("unknown-app", None)), None);
    }

    #[test]
    fn title_pattern_must_match() {
        let mut m = AppMatcher::new();
        m.add(AppId(1), vec!["code".into()], Some(".*\\.rs")).unwrap();
        assert_eq!(m.resolve(&identity("code", Some("main.rs — project"))), Some(AppId(1)));
        assert_eq!(m.resolve(&identity("code", Some("index.ts — project"))), None);
    }

    #[test]
    fn title_pattern_without_window_title_does_not_match() {
        let mut m = AppMatcher::new();
        m.add(AppId(1), vec!["code".into()], Some(".*\\.rs")).unwrap();
        assert_eq!(m.resolve(&identity("code", None)), None);
    }

    #[test]
    fn no_title_pattern_matches_any_title() {
        let mut m = AppMatcher::new();
        m.add(AppId(1), vec!["code".into()], None).unwrap();
        assert_eq!(m.resolve(&identity("code", Some("anything"))), Some(AppId(1)));
        assert_eq!(m.resolve(&identity("code", None)), Some(AppId(1)));
    }

    #[test]
    fn first_matching_entry_wins() {
        let mut m = AppMatcher::new();
        m.add(AppId(1), vec!["code".into()], Some("project-a")).unwrap();
        m.add(AppId(2), vec!["code".into()], None).unwrap();
        assert_eq!(m.resolve(&identity("code", Some("project-a"))), Some(AppId(1)));
        assert_eq!(m.resolve(&identity("code", Some("project-b"))), Some(AppId(2)));
    }
}
