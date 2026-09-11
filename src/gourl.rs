//! Minimal stand-in for Go's `net/url`, covering what hey parses: the target
//! URL and the `-x` proxy address.

use std::fmt;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub user: Option<String>,
    pub password: Option<String>,
    pub host: String, // host[:port], as written
    pub path: String,
    pub raw_query: Option<String>,
    pub fragment: Option<String>,
    /// True when the input had no "scheme://" prefix (Go calls this an opaque
    /// or relative reference); hey ends up reporting an unsupported scheme.
    pub relative: bool,
}

impl Url {
    /// Host without the port, suitable for TLS SNI and the Host header.
    pub fn hostname(&self) -> String {
        let h = &self.host;
        if let Some(rest) = h.strip_prefix('[') {
            // IPv6 literal
            if let Some(end) = rest.find(']') {
                return rest[..end].to_string();
            }
        }
        match h.rfind(':') {
            Some(i) if h[i + 1..].chars().all(|c| c.is_ascii_digit()) => h[..i].to_string(),
            _ => h.clone(),
        }
    }

    /// The raw port substring as written, which Go keeps even when it is out
    /// of range -- `url.Parse` only checks that it is all digits, and the
    /// range check happens later at dial time.
    pub fn raw_port(&self) -> Option<&str> {
        let h = &self.host;
        let idx = if h.starts_with('[') {
            h.find(']').and_then(|e| {
                if h[e + 1..].starts_with(':') {
                    Some(e + 1)
                } else {
                    None
                }
            })
        } else {
            h.rfind(':')
        }?;
        let p = &h[idx + 1..];
        if p.chars().all(|c| c.is_ascii_digit()) {
            Some(p)
        } else {
            None
        }
    }

    pub fn port(&self) -> Option<u16> {
        self.raw_port().and_then(|p| p.parse::<u16>().ok())
    }

    /// host:port with the scheme's default port filled in.
    pub fn authority(&self) -> String {
        match self.raw_port() {
            Some(_) => self.host.clone(),
            None => {
                let p = if self.scheme == "https" { 443 } else { 80 };
                format!("{}:{}", self.host, p)
            }
        }
    }

    /// Path + query, i.e. the origin-form request target.
    pub fn request_uri(&self) -> String {
        let mut p = if self.path.is_empty() {
            "/".to_string()
        } else {
            self.path.clone()
        };
        if let Some(q) = &self.raw_query {
            p.push('?');
            p.push_str(q);
        }
        p
    }

    /// Go: `(*URL).ResolveReference` -- used when following redirects.
    pub fn resolve_reference(&self, refr: &str) -> Result<Url, String> {
        if let Ok(u) = parse(refr) {
            if !u.scheme.is_empty() {
                return Ok(u);
            }
        }
        let mut out = self.clone();
        out.fragment = None;
        if let Some(rest) = refr.strip_prefix("//") {
            // network-path reference: keep our scheme, replace authority
            let parsed = parse(&format!("{}://{}", self.scheme, rest))?;
            return Ok(parsed);
        }
        let (target, query) = match refr.find('?') {
            Some(i) => (&refr[..i], Some(refr[i + 1..].to_string())),
            None => (refr, None),
        };
        if target.is_empty() {
            out.raw_query = query.or(out.raw_query);
            return Ok(out);
        }
        out.raw_query = query;
        if target.starts_with('/') {
            out.path = remove_dot_segments(target);
        } else {
            let base = match self.path.rfind('/') {
                Some(i) => &self.path[..=i],
                None => "/",
            };
            out.path = remove_dot_segments(&format!("{}{}", base, target));
        }
        Ok(out)
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.relative {
            let mut s = self.path.clone();
            if let Some(q) = &self.raw_query {
                s.push('?');
                s.push_str(q);
            }
            return write!(f, "{}", s);
        }
        // Go's URL.String only emits "//" when there is something to put
        // after it, so {Scheme:"http", Host:"", Path:""} prints as "http:".
        write!(f, "{}:", self.scheme)?;
        if !self.host.is_empty() || !self.path.is_empty() || self.user.is_some() {
            write!(f, "//")?;
        }
        if let Some(u) = &self.user {
            write!(f, "{}", u)?;
            if self.password.is_some() {
                // Go's stripPassword replaces the password with "***" in
                // url.Error messages.
                write!(f, ":***")?;
            }
            write!(f, "@")?;
        }
        write!(f, "{}", self.host)?;
        let p = if self.path.is_empty() && self.raw_query.is_none() {
            String::new()
        } else if self.path.is_empty() {
            "/".to_string()
        } else {
            self.path.clone()
        };
        write!(f, "{}", p)?;
        if let Some(q) = &self.raw_query {
            write!(f, "?{}", q)?;
        }
        if let Some(fr) = &self.fragment {
            write!(f, "#{}", fr)?;
        }
        Ok(())
    }
}

fn remove_dot_segments(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let trailing = p.ends_with('/') || p.ends_with("/.") || p.ends_with("/..");
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let mut s = String::from("/");
    s.push_str(&out.join("/"));
    if trailing && !s.ends_with('/') {
        s.push('/');
    }
    s
}

/// Go: `url.Parse`
pub fn parse(raw: &str) -> Result<Url, String> {
    let mut u = Url::default();
    let mut rest = raw;

    // Fragment
    if let Some(i) = rest.find('#') {
        u.fragment = Some(rest[i + 1..].to_string());
        rest = &rest[..i];
    }

    // Scheme
    let scheme_end = rest.find("://");
    match scheme_end {
        Some(i) if is_scheme(&rest[..i]) => {
            u.scheme = rest[..i].to_ascii_lowercase();
            rest = &rest[i + 3..];
        }
        _ => {
            // Reject things Go rejects outright, e.g. "://host".
            if rest.starts_with("://") {
                return Err(format!("parse {:?}: missing protocol scheme", raw));
            }
            if let Some(i) = rest.find(':') {
                let head = &rest[..i];
                if !head.is_empty()
                    && !is_scheme(head)
                    && head.chars().next().unwrap().is_ascii_digit()
                {
                    return Err(format!(
                        "parse {:?}: first path segment in URL cannot contain colon",
                        raw
                    ));
                }
            }
            u.relative = true;
            let (p, q) = split_query(rest);
            u.path = p.to_string();
            u.raw_query = q;
            return Ok(u);
        }
    }

    // Authority
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    rest = &rest[auth_end..];

    let hostpart = match authority.rfind('@') {
        Some(i) => {
            let userinfo = &authority[..i];
            match userinfo.find(':') {
                Some(j) => {
                    u.user = Some(userinfo[..j].to_string());
                    u.password = Some(userinfo[j + 1..].to_string());
                }
                None => u.user = Some(userinfo.to_string()),
            }
            &authority[i + 1..]
        }
        None => authority,
    };
    // Note: Go accepts an empty authority ("http://"); the failure surfaces
    // later as "http: no Host in request URL" from the Transport.
    // Validate the port if present.
    let hp = hostpart.to_string();
    if !hp.starts_with('[') {
        if let Some(i) = hp.rfind(':') {
            let port = &hp[i + 1..];
            if !port.is_empty() && !port.chars().all(|c| c.is_ascii_digit()) {
                return Err(format!(
                    "parse {:?}: invalid port {:?} after host",
                    raw,
                    format!(":{}", port)
                ));
            }
        }
    }
    u.host = hp;

    let (p, q) = split_query(rest);
    u.path = p.to_string();
    u.raw_query = q;
    Ok(u)
}

fn split_query(s: &str) -> (&str, Option<String>) {
    match s.find('?') {
        Some(i) => (&s[..i], Some(s[i + 1..].to_string())),
        None => (s, None),
    }
}

fn is_scheme(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().unwrap().is_ascii_alphabetic()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
}

#[cfg(test)]
#[path = "gourl_test.rs"]
mod gourl_test;
