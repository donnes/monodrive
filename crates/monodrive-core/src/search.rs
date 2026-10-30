//! The search language the window and `monodrive mcp` share: words for
//! fsearch's name matching plus typed filters, such as
//! `report kind:document size:>1mb modified:7d in:~/Work`. A `Query` prints
//! back as the text it parsed from, so chips can rebuild the search field.
//! `index` runs one; docs/search.md has the rules. A `Search` is one as the
//! window runs it: what's typed, whether it looks at names or inside files,
//! and where.

use crate::expand_home;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

/// A parsed search: its words and filters in the order typed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub terms: Vec<Term>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Term {
    /// Anything else, for fsearch as typed: a fuzzy word, `'exact`, `!not`,
    /// a glob such as `src/**/*.rs`, or one of fsearch's other filters.
    Word(String),
    /// Any of these kinds.
    Kind(Vec<Kind>),
    /// Any of these extensions, lowercase and without the dot.
    Ext(Vec<String>),
    Size(Bound<Amount>),
    Modified(Modified),
    /// The folder to search under, as typed.
    In(PathBuf),
    /// Text to look for inside files, as `grep` does, which makes the rest of
    /// the query pick the files read. It's how a kept content search says so.
    Contains(String),
}

/// What `kind:` names. The groups are fsearch's `type:` extension lists,
/// since the index holds names and a UTType per file would cost a Launch
/// Services lookup per hit. Folder, file and link are the entry's own kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
    Audio,
    Document,
    Code,
    Archive,
    Font,
    App,
    Folder,
    File,
    Link,
}

/// A comparison as typed: `>x`, `>=x`, `<x`, `<=x`, `a..b` with both ends
/// included, or `x`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound<T> {
    Over(T),
    AtLeast(T),
    Under(T),
    AtMost(T),
    Between(T, T),
    Is(T),
}

/// A size in Finder's decimal units: 1 kb is 1,000 bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Amount {
    pub n: f64,
    pub unit: Unit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    B,
    Kb,
    Mb,
    Gb,
    Tb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modified {
    /// `7d` or `<7d`: in the last 7 days.
    Within(Age),
    /// `>7d`: longer ago than that.
    OlderThan(Age),
    /// Whole days in local time: `today`, `>=2026-01-01`, `2026-01-01..2026-01-31`.
    Days(Bound<Day>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Age {
    pub n: u32,
    pub unit: AgeUnit,
}

/// `h`, `d`, `w`, `mo` and `y`. A month is 30 days and a year 365, as in fsearch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgeUnit {
    Hours,
    Days,
    Weeks,
    Months,
    Years,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Day {
    Today,
    Yesterday,
    Date { year: i32, month: u8, day: u8 },
}

/// What a search looks at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Looks {
    #[default]
    Names,
    Contents,
}

/// A search as the window runs it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Search {
    /// What's typed. Looking at contents, its plain words are the text to
    /// look for, one phrase, and its filters pick the files read.
    pub text: String,
    pub looks: Looks,
    pub scope: Scope,
}

impl Search {
    /// The text a kept search stores, canonical, with `contains:` saying it
    /// looks inside files, since `SavedSearch` and `RecentSearch` hold a query.
    pub fn stored(&self) -> Result<String, String> {
        let (pattern, filters) = self.parts()?;
        let contains = pattern.map(Term::Contains);
        Ok(Query {
            terms: contains.into_iter().chain(filters.terms).collect(),
        }
        .to_string())
    }

    /// What `stored` kept, ready to type and run again. Words beside a
    /// `contains:` pick files by name, which only the token can say, so that
    /// stays as it's written.
    pub fn from_stored(query: &str, scope: Scope) -> Search {
        let mut search = Search {
            text: query.into(),
            looks: Looks::Names,
            scope,
        };
        if let Ok(parsed) = Query::parse(query)
            && parsed.contains().is_some()
            && !parsed.terms.iter().any(|t| matches!(t, Term::Word(_)))
        {
            search.looks = Looks::Contents;
            search.text = parsed
                .terms
                .iter()
                .map(|term| match term {
                    Term::Contains(text) => text.clone(),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
        search
    }

    /// The text to look for inside files, if this looks inside files and has
    /// any, and the query that's left to pick files with. Without any, it's
    /// a name search, so filters alone still find something.
    pub(crate) fn parts(&self) -> Result<(Option<String>, Query), String> {
        let query = Query::parse(&self.text)?;
        if let Some(text) = query.contains() {
            let pattern = text.to_string();
            let terms = query
                .terms
                .into_iter()
                .filter(|term| !matches!(term, Term::Contains(_)))
                .collect();
            return Ok((Some(pattern), Query { terms }));
        }
        if self.looks == Looks::Names {
            return Ok((None, query));
        }
        let mut words = Vec::new();
        let mut terms = Vec::new();
        for term in query.terms {
            match term {
                Term::Word(word) => words.push(word),
                other => terms.push(other),
            }
        }
        Ok((
            (!words.is_empty()).then(|| words.join(" ")),
            Query { terms },
        ))
    }
}

/// Where a search looks when its query has no `in:`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Every local folder the index holds.
    #[default]
    Everywhere,
    /// Everything under this folder.
    Folder(PathBuf),
}

const KIND_HELP: &str = "kinds are image, video, audio, document, code, archive, font, app, \
folder, file and link";

impl Query {
    /// Parses what was typed. An error names the token and says why.
    pub fn parse(text: &str) -> Result<Query, String> {
        let query = Query {
            terms: words(text)
                .iter()
                .map(|word| term(word))
                .collect::<Result<_, _>>()?,
        };
        query.check()?;
        Ok(query)
    }

    /// Whether fsearch can answer it: one `size:`, `modified:` and `in:`
    /// at most, and folder, file, link or app only as the one kind.
    pub fn check(&self) -> Result<(), String> {
        let once = |is: fn(&Term) -> bool, hint: &str| match self
            .terms
            .iter()
            .filter(|term| is(term))
            .nth(1)
        {
            Some(second) => Err(format!("{second}: a search takes one; {hint}")),
            None => Ok(()),
        };
        once(
            |t| matches!(t, Term::Size(_)),
            "give a range such as size:1mb..10mb",
        )?;
        once(
            |t| matches!(t, Term::Modified(_)),
            "give a range such as modified:2026-01-01..2026-01-31",
        )?;
        once(|t| matches!(t, Term::In(_)), "search one folder")?;
        once(
            |t| matches!(t, Term::Contains(_)),
            "give one phrase in quotes",
        )?;
        let kinds: Vec<Kind> = self
            .terms
            .iter()
            .flat_map(|term| match term {
                Term::Kind(kinds) => kinds.as_slice(),
                _ => &[],
            })
            .copied()
            .collect();
        match kinds.iter().find(|kind| kind.alone()) {
            Some(alone) if kinds.len() > 1 => Err(format!(
                "kind:{}: can't be combined with another kind",
                alone.name()
            )),
            _ => Ok(()),
        }
    }

    /// The text its `contains:` looks for inside files.
    pub fn contains(&self) -> Option<&str> {
        self.terms.iter().find_map(|term| match term {
            Term::Contains(text) => Some(text.as_str()),
            _ => None,
        })
    }

    /// The folder it searches under: its `in:`, from the scope's folder
    /// when relative, or else the scope's.
    pub fn folder(&self, scope: &Scope) -> Result<Option<PathBuf>, String> {
        let base = match scope {
            Scope::Everywhere => None,
            Scope::Folder(folder) => Some(folder),
        };
        let Some(typed) = self.terms.iter().find_map(|term| match term {
            Term::In(path) => Some(path),
            _ => None,
        }) else {
            return Ok(base.cloned());
        };
        let path = expand_home(&typed.to_string_lossy());
        if path.is_absolute() {
            return Ok(Some(path));
        }
        base.map(|base| Some(base.join(&path))).ok_or_else(|| {
            format!(
                "in:{}: give a full path or one starting with ~",
                typed.display()
            )
        })
    }
}

fn term(word: &str) -> Result<Term, String> {
    let Some((key, value)) = word.split_once(':') else {
        return Ok(Term::Word(word.into()));
    };
    let lower = value.to_lowercase();
    let (term, why) = match key {
        "kind" | "type" => (
            lower
                .split(',')
                .map(Kind::parse)
                .collect::<Option<_>>()
                .map(Term::Kind),
            KIND_HELP,
        ),
        "ext" => (
            lower
                .split(',')
                .map(|ext| ext.trim_start_matches('.'))
                .map(|ext| (!ext.is_empty()).then(|| ext.to_string()))
                .collect::<Option<_>>()
                .map(Term::Ext),
            "give extensions such as rs or jpg,png",
        ),
        "size" => (
            Bound::parse(&lower, Amount::parse).map(Term::Size),
            "give a size such as >10mb, <=1gb or 1mb..10mb",
        ),
        "modified" | "mtime" => (
            Modified::parse(&lower).map(Term::Modified),
            "give an age such as 7d or >1y, or days such as today, yesterday, \
             >=2026-01-01 or 2026-01-01..2026-01-31",
        ),
        "in" => (
            (!value.is_empty()).then(|| Term::In(value.into())),
            "give a folder such as ~/Work",
        ),
        "contains" => (
            (!value.is_empty()).then(|| Term::Contains(value.into())),
            "give the text to look for inside files",
        ),
        _ => return Ok(Term::Word(word.into())),
    };
    term.ok_or_else(|| format!("{word}: {why}"))
}

/// The words of `text` as fsearch splits them: at spaces, except inside
/// double quotes, which are dropped.
fn words(text: &str) -> Vec<String> {
    let (mut out, mut word, mut quoted) = (Vec::new(), String::new(), false);
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
            }
            _ => word.push(c),
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

impl Kind {
    pub const ALL: [Kind; 11] = [
        Kind::Image,
        Kind::Video,
        Kind::Audio,
        Kind::Document,
        Kind::Code,
        Kind::Archive,
        Kind::Font,
        Kind::App,
        Kind::Folder,
        Kind::File,
        Kind::Link,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::Document => "document",
            Kind::Code => "code",
            Kind::Archive => "archive",
            Kind::Font => "font",
            Kind::App => "app",
            Kind::Folder => "folder",
            Kind::File => "file",
            Kind::Link => "link",
        }
    }

    fn parse(name: &str) -> Option<Kind> {
        let name = match name {
            "doc" | "docs" => "document",
            "dir" | "directory" => "folder",
            "symlink" => "link",
            name => name,
        };
        Kind::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// Sets the entry kind fsearch matches, which it can't widen with
    /// another kind (an app is a folder named `.app`).
    fn alone(self) -> bool {
        matches!(self, Kind::App | Kind::Folder | Kind::File | Kind::Link)
    }
}

impl<T> Bound<T> {
    fn parse(text: &str, value: impl Fn(&str) -> Option<T>) -> Option<Self> {
        if let Some(x) = text.strip_prefix(">=") {
            return value(x).map(Self::AtLeast);
        }
        if let Some(x) = text.strip_prefix('>') {
            return value(x).map(Self::Over);
        }
        if let Some(x) = text.strip_prefix("<=") {
            return value(x).map(Self::AtMost);
        }
        if let Some(x) = text.strip_prefix('<') {
            return value(x).map(Self::Under);
        }
        if let Some((a, b)) = text.split_once("..") {
            return Some(Self::Between(value(a)?, value(b)?));
        }
        value(text).map(Self::Is)
    }

    /// The first and last values it takes, both included, from the first
    /// and last each `T` covers.
    fn span(&self, first: impl Fn(&T) -> i64, last: impl Fn(&T) -> i64) -> (i64, i64) {
        match self {
            Bound::Over(x) => (last(x).saturating_add(1), i64::MAX),
            Bound::AtLeast(x) => (first(x), i64::MAX),
            Bound::Under(x) => (i64::MIN, first(x).saturating_sub(1)),
            Bound::AtMost(x) => (i64::MIN, last(x)),
            Bound::Between(a, b) => (first(a), last(b)),
            Bound::Is(x) => (first(x), last(x)),
        }
    }
}

impl Bound<Amount> {
    /// The sizes it takes in bytes, both ends included.
    pub fn bytes(&self) -> (i64, i64) {
        self.span(|a| a.bytes(), |a| a.bytes())
    }
}

impl Amount {
    fn parse(text: &str) -> Option<Self> {
        let digits = text
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(text.len());
        let unit = match &text[digits..] {
            "" | "b" => Unit::B,
            "k" | "kb" => Unit::Kb,
            "m" | "mb" => Unit::Mb,
            "g" | "gb" => Unit::Gb,
            "t" | "tb" => Unit::Tb,
            _ => return None,
        };
        let n = text[..digits].parse().ok()?;
        Some(Amount { n, unit })
    }

    fn bytes(&self) -> i64 {
        let per = match self.unit {
            Unit::B => 1.,
            Unit::Kb => 1e3,
            Unit::Mb => 1e6,
            Unit::Gb => 1e9,
            Unit::Tb => 1e12,
        };
        // Saturates past i64::MAX.
        (self.n * per) as i64
    }
}

impl Modified {
    fn parse(text: &str) -> Option<Self> {
        let newer = text.strip_prefix("<=").or_else(|| text.strip_prefix('<'));
        if let Some(age) = newer.or(Some(text)).and_then(Age::parse) {
            return Some(Modified::Within(age));
        }
        let older = text.strip_prefix(">=").or_else(|| text.strip_prefix('>'));
        if let Some(age) = older.and_then(Age::parse) {
            return Some(Modified::OlderThan(age));
        }
        Bound::parse(text, Day::parse).map(Modified::Days)
    }

    /// The first and last second it takes at `now`, both included, counted
    /// from the epoch.
    pub fn seconds(&self, now: i64) -> (i64, i64) {
        match self {
            Modified::Within(age) => (now.saturating_sub(age.seconds()), i64::MAX),
            Modified::OlderThan(age) => (i64::MIN, now.saturating_sub(age.seconds())),
            Modified::Days(days) => days.span(
                |day| day.start(now, 0),
                |day| day.start(now, 1).saturating_sub(1),
            ),
        }
    }
}

impl Age {
    fn parse(text: &str) -> Option<Self> {
        let digits = text
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len());
        let unit = match &text[digits..] {
            "h" => AgeUnit::Hours,
            "d" => AgeUnit::Days,
            "w" => AgeUnit::Weeks,
            "mo" => AgeUnit::Months,
            "y" => AgeUnit::Years,
            _ => return None,
        };
        Some(Age {
            n: text[..digits].parse().ok()?,
            unit,
        })
    }

    fn seconds(&self) -> i64 {
        let per = match self.unit {
            AgeUnit::Hours => 3600,
            AgeUnit::Days => 86_400,
            AgeUnit::Weeks => 7 * 86_400,
            AgeUnit::Months => 30 * 86_400,
            AgeUnit::Years => 365 * 86_400,
        };
        i64::from(self.n) * per
    }
}

impl Day {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "today" => return Some(Day::Today),
            "yesterday" => return Some(Day::Yesterday),
            _ => {}
        }
        let mut parts = text.splitn(3, '-');
        let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
        if year.len() != 4 || month.len() != 2 || day.len() != 2 {
            return None;
        }
        let (year, month, day): (i32, u8, u8) =
            (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            1..=12 => 31,
            _ => return None,
        };
        (1..=days)
            .contains(&day)
            .then_some(Day::Date { year, month, day })
    }

    /// Local midnight `after` days past this day's, in seconds from the
    /// epoch, so a day across a daylight saving change keeps its length.
    fn start(&self, now: i64, after: i32) -> i64 {
        // SAFETY: an all-zero tm is valid; localtime_r and mktime write only
        // into it.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&now, &mut tm);
            match *self {
                Day::Today => {}
                Day::Yesterday => tm.tm_mday -= 1,
                Day::Date { year, month, day } => {
                    tm.tm_year = year - 1900;
                    tm.tm_mon = i32::from(month) - 1;
                    tm.tm_mday = i32::from(day);
                }
            }
            tm.tm_mday += after;
            (tm.tm_hour, tm.tm_min, tm.tm_sec, tm.tm_isdst) = (0, 0, 0, -1);
            libc::mktime(&mut tm)
        }
    }
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, term) in self.terms.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{term}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Term {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Quoted where a space would split it.
        let quoted = |f: &mut fmt::Formatter<'_>, key: &str, value: &str| {
            if value.contains(' ') {
                write!(f, "{key}\"{value}\"")
            } else {
                write!(f, "{key}{value}")
            }
        };
        match self {
            Term::Word(word) => quoted(f, "", word),
            Term::Kind(kinds) => {
                let names: Vec<&str> = kinds.iter().map(|kind| kind.name()).collect();
                write!(f, "kind:{}", names.join(","))
            }
            Term::Ext(exts) => write!(f, "ext:{}", exts.join(",")),
            Term::Size(size) => write!(f, "size:{size}"),
            Term::Modified(modified) => write!(f, "modified:{modified}"),
            Term::In(path) => quoted(f, "in:", &path.to_string_lossy()),
            Term::Contains(text) => quoted(f, "contains:", text),
        }
    }
}

impl<T: fmt::Display> fmt::Display for Bound<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Bound::Over(x) => write!(f, ">{x}"),
            Bound::AtLeast(x) => write!(f, ">={x}"),
            Bound::Under(x) => write!(f, "<{x}"),
            Bound::AtMost(x) => write!(f, "<={x}"),
            Bound::Between(a, b) => write!(f, "{a}..{b}"),
            Bound::Is(x) => write!(f, "{x}"),
        }
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let unit = match self.unit {
            Unit::B => "b",
            Unit::Kb => "kb",
            Unit::Mb => "mb",
            Unit::Gb => "gb",
            Unit::Tb => "tb",
        };
        write!(f, "{}{unit}", self.n)
    }
}

impl fmt::Display for Modified {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Modified::Within(age) => write!(f, "{age}"),
            Modified::OlderThan(age) => write!(f, ">{age}"),
            Modified::Days(days) => write!(f, "{days}"),
        }
    }
}

impl fmt::Display for Age {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let unit = match self.unit {
            AgeUnit::Hours => "h",
            AgeUnit::Days => "d",
            AgeUnit::Weeks => "w",
            AgeUnit::Months => "mo",
            AgeUnit::Years => "y",
        };
        write!(f, "{}{unit}", self.n)
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Day::Today => f.write_str("today"),
            Day::Yesterday => f.write_str("yesterday"),
            Day::Date { year, month, day } => write!(f, "{year:04}-{month:02}-{day:02}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(text: &str) -> String {
        Query::parse(text).unwrap().to_string()
    }

    #[test]
    fn every_filter_parses_and_prints_back_as_its_canonical_text() {
        let query = Query::parse(
            "report kind:image,video ext:.JPG,png size:>=1.5MB modified:<7d \
             in:\"~/My Photos\" !draft",
        )
        .unwrap();
        let amount = |n, unit| Amount { n, unit };
        assert_eq!(
            query.terms,
            [
                Term::Word("report".into()),
                Term::Kind(vec![Kind::Image, Kind::Video]),
                Term::Ext(vec!["jpg".into(), "png".into()]),
                Term::Size(Bound::AtLeast(amount(1.5, Unit::Mb))),
                Term::Modified(Modified::Within(Age {
                    n: 7,
                    unit: AgeUnit::Days
                })),
                Term::In("~/My Photos".into()),
                Term::Word("!draft".into()),
            ]
        );
        let text = query.to_string();
        assert_eq!(
            text,
            "report kind:image,video ext:jpg,png size:>=1.5mb modified:7d \
             in:\"~/My Photos\" !draft"
        );
        assert_eq!(Query::parse(&text).unwrap(), query);

        for (typed, canonical) in [
            ("type:doc", "kind:document"),
            ("kind:dir", "kind:folder"),
            ("kind:App", "kind:app"),
            ("size:<10kb", "size:<10kb"),
            ("size:<=1g", "size:<=1gb"),
            ("size:>100", "size:>100b"),
            ("size:1mb..10mb", "size:1mb..10mb"),
            ("size:2tb", "size:2tb"),
            ("mtime:>1y", "modified:>1y"),
            ("modified:12h", "modified:12h"),
            ("modified:<=2w", "modified:2w"),
            ("modified:3mo", "modified:3mo"),
            ("modified:today", "modified:today"),
            ("modified:>=Yesterday", "modified:>=yesterday"),
            ("modified:<2026-01-01", "modified:<2026-01-01"),
            ("modified:2024-02-29", "modified:2024-02-29"),
            (
                "modified:2026-01-01..2026-01-31",
                "modified:2026-01-01..2026-01-31",
            ),
            (
                "\"two words\" re:^a src/**/*.rs",
                "\"two words\" re:^a src/**/*.rs",
            ),
            ("  spaced    out  ", "spaced out"),
            ("", ""),
        ] {
            assert_eq!(shown(typed), canonical, "{typed}");
            assert_eq!(shown(canonical), canonical, "{canonical}");
        }
    }

    #[test]
    fn a_content_search_keeps_its_phrase_and_filters_and_comes_back_as_typed() {
        let contents = |text: &str| Search {
            text: text.into(),
            looks: Looks::Contents,
            scope: Scope::Everywhere,
        };
        let search = contents("fix me kind:code");
        let stored = search.stored().unwrap();
        assert_eq!(stored, "contains:\"fix me\" kind:code");
        // Plain words are the phrase; only filters pick the files.
        let (pattern, filters) = search.parts().unwrap();
        assert_eq!(pattern.as_deref(), Some("fix me"));
        assert_eq!(filters.to_string(), "kind:code");
        // Stored, it parses as a saved search's query does, and runs the same.
        assert_eq!(Query::parse(&stored).unwrap().to_string(), stored);
        let back = Search::from_stored(&stored, Scope::Everywhere);
        assert_eq!(back.looks, Looks::Contents);
        assert_eq!(back.text, "fix me kind:code");
        assert_eq!(back.parts().unwrap(), search.parts().unwrap());

        // Filters alone have nothing to look for, so they find by name.
        let (pattern, filters) = contents("kind:image").parts().unwrap();
        assert_eq!(pattern, None);
        assert_eq!(filters.to_string(), "kind:image");

        // A name search stays one, and its words stay words.
        let names = Search {
            text: "Report   kind:doc".into(),
            ..Search::default()
        };
        assert_eq!(names.stored().unwrap(), "Report kind:document");
        let back = Search::from_stored("Report kind:document", Scope::Everywhere);
        assert_eq!(back.looks, Looks::Names);
        assert_eq!(back.text, "Report kind:document");

        // Typed by hand it looks inside files in either mode, and the words
        // beside it pick the files by name, so it keeps the token.
        let (pattern, filters) = contents("contains:todo notes").parts().unwrap();
        assert_eq!(pattern.as_deref(), Some("todo"));
        assert_eq!(filters.to_string(), "notes");
        let stored = contents("contains:todo notes").stored().unwrap();
        let back = Search::from_stored(&stored, Scope::Everywhere);
        assert_eq!(back.text, "contains:todo notes");
        assert_eq!(back.parts().unwrap().0.as_deref(), Some("todo"));

        assert_eq!(
            contents("size:big").stored().unwrap_err(),
            "size:big: give a size such as >10mb, <=1gb or 1mb..10mb"
        );
    }

    #[test]
    fn a_bad_filter_names_its_token_and_says_why() {
        for (typed, error) in [
            ("contains:", "contains:: give the text"),
            ("contains:a contains:b", "contains:b: a search takes one"),
            ("kind:bogus", "kind:bogus: kinds are image, video"),
            ("kind:", "kind:: kinds are"),
            ("ext:", "ext:: give extensions"),
            ("ext:rs,", "ext:rs,: give extensions"),
            ("size:>1zb", "size:>1zb: give a size"),
            ("size:big", "size:big: give a size"),
            ("modified:soon", "modified:soon: give an age"),
            ("modified:2026", "modified:2026: give an age"),
            ("modified:2026-02-29", "modified:2026-02-29: give an age"),
            ("modified:2026-13-01", "modified:2026-13-01: give an age"),
            ("in:", "in:: give a folder"),
            (
                "size:>1mb size:<10mb",
                "size:<10mb: a search takes one; give a range such as size:1mb..10mb",
            ),
            (
                "modified:today modified:7d",
                "modified:7d: a search takes one",
            ),
            (
                "in:~/a in:~/b",
                "in:~/b: a search takes one; search one folder",
            ),
            (
                "kind:image kind:folder",
                "kind:folder: can't be combined with another kind",
            ),
            ("kind:app,code", "kind:app: can't be combined"),
        ] {
            let got = Query::parse(typed).unwrap_err();
            assert!(got.starts_with(error), "{typed}: {got}");
        }
    }

    #[test]
    fn sizes_are_decimal_and_each_comparison_keeps_its_edge() {
        let bytes = |text: &str| match &Query::parse(text).unwrap().terms[0] {
            Term::Size(size) => size.bytes(),
            other => panic!("{other:?}"),
        };
        assert_eq!(bytes("size:>1kb"), (1001, i64::MAX));
        assert_eq!(bytes("size:>=1kb"), (1000, i64::MAX));
        assert_eq!(bytes("size:<1.5mb"), (i64::MIN, 1_499_999));
        assert_eq!(bytes("size:<=1gb"), (i64::MIN, 1_000_000_000));
        assert_eq!(bytes("size:1mb..2mb"), (1_000_000, 2_000_000));
        assert_eq!(bytes("size:512"), (512, 512));
    }

    #[test]
    fn ages_count_back_from_now_and_days_are_whole_local_days() {
        let seconds = |text: &str, now| match &Query::parse(text).unwrap().terms[0] {
            Term::Modified(modified) => modified.seconds(now),
            other => panic!("{other:?}"),
        };
        let now = 1_800_000_000;
        assert_eq!(seconds("modified:7d", now), (now - 7 * 86_400, i64::MAX));
        assert_eq!(seconds("modified:>2h", now), (i64::MIN, now - 7200));

        let midnight = Day::Today.start(now, 0);
        let tomorrow = Day::Today.start(now, 1);
        assert!(midnight <= now && now < tomorrow);
        assert!((82_800..=90_000).contains(&(tomorrow - midnight)));
        assert_eq!(seconds("modified:today", now), (midnight, tomorrow - 1));
        assert_eq!(
            seconds("modified:yesterday", now),
            (Day::Yesterday.start(now, 0), midnight - 1)
        );
        assert_eq!(seconds("modified:>yesterday", now), (midnight, i64::MAX));
        assert_eq!(seconds("modified:<today", now), (i64::MIN, midnight - 1));

        // Local midnight on the date, whatever day it is now.
        let jan1 = Day::Date {
            year: 2026,
            month: 1,
            day: 1,
        };
        let start = jan1.start(now, 0);
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&start, &mut tm) };
        assert_eq!(
            (tm.tm_year, tm.tm_mon, tm.tm_mday, tm.tm_hour, tm.tm_min),
            (126, 0, 1, 0, 0)
        );
        assert_eq!(
            seconds("modified:2026-01-01..2026-01-31", now),
            (start, start + 31 * 86_400 - 1)
        );
        assert_eq!(
            seconds("modified:<=2026-01-01", now),
            (i64::MIN, start + 86_400 - 1)
        );
        assert_eq!(seconds("modified:>=2026-01-01", 0), (start, i64::MAX));
    }

    #[test]
    fn in_wins_over_the_scope_and_a_relative_one_starts_from_its_folder() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let work = Scope::Folder("/Volumes/Work".into());
        let folder = |text: &str, scope: &Scope| Query::parse(text).unwrap().folder(scope);
        assert_eq!(folder("notes", &Scope::Everywhere), Ok(None));
        assert_eq!(folder("notes", &work), Ok(Some("/Volumes/Work".into())));
        assert_eq!(folder("in:/tmp", &work), Ok(Some("/tmp".into())));
        assert_eq!(
            folder("in:~/Pictures", &work),
            Ok(Some(home.join("Pictures")))
        );
        assert_eq!(
            folder("in:src", &work),
            Ok(Some("/Volumes/Work/src".into()))
        );
        assert_eq!(
            folder("in:src", &Scope::Everywhere),
            Err("in:src: give a full path or one starting with ~".into())
        );
    }
}
