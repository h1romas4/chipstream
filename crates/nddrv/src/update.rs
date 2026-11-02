use semver::Version;
use serde::Deserialize;
use std::time::Duration;

const RELEASES_API: &str = "https://api.github.com/repos/h1romas4/chipstream/releases";

pub fn check() -> anyhow::Result<Option<Version>> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();
    check_releases(&agent, RELEASES_API, &current)
}

fn check_releases(
    agent: &ureq::Agent,
    api: &str,
    current: &Version,
) -> anyhow::Result<Option<Version>> {
    let mut newest = None;
    let mut page = 1;
    loop {
        let releases: Vec<Release> = agent
            .get(format!("{api}?per_page=100&page={page}"))
            .header("User-Agent", concat!("nddrv/", env!("CARGO_PKG_VERSION")))
            .header("Accept", "application/vnd.github+json")
            .call()?
            .body_mut()
            .read_json()?;
        if let Some(version) = latest_version(&releases, current) {
            newest =
                Some(newest.map_or(version.clone(), |previous: Version| previous.max(version)));
        }
        if releases.len() < 100 {
            return Ok(newest);
        }
        page += 1;
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

fn latest_version(releases: &[Release], current: &Version) -> Option<Version> {
    releases
        .iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| release.tag_name.strip_prefix("nddrv-v"))
        .filter_map(|version| Version::parse(version).ok())
        .filter(|version| version.pre.is_empty() && version > current)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn serve_responses(responses: Vec<String>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/releases", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for (index, body) in responses.into_iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.ends_with(b"\r\n\r\n") {
                    let size = stream.read(&mut buffer).unwrap();
                    assert!(size > 0);
                    request.extend_from_slice(&buffer[..size]);
                }
                let request = String::from_utf8_lossy(&request);
                assert!(
                    request.contains(&format!("GET /releases?per_page=100&page={} ", index + 1))
                );
                assert!(request.to_ascii_lowercase().contains("user-agent: nddrv/"));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        (url, server)
    }

    fn test_agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .into()
    }

    #[test]
    fn selects_only_newer_stable_nddrv_releases() {
        let releases: Vec<Release> = serde_json::from_str(
            r#"[
                {"tag_name":"other-v9.0.0","draft":false,"prerelease":false},
                {"tag_name":"nddrv-v0.3.0","draft":true,"prerelease":false},
                {"tag_name":"nddrv-v0.4.0-rc.1","draft":false,"prerelease":true},
                {"tag_name":"nddrv-v0.2.0","draft":false,"prerelease":false},
                {"tag_name":"nddrv-v0.1.9","draft":false,"prerelease":false},
                {"tag_name":"nddrv-vinvalid","draft":false,"prerelease":false}
            ]"#,
        )
        .unwrap();
        assert_eq!(
            latest_version(&releases, &Version::parse("0.1.0").unwrap()),
            Some(Version::parse("0.2.0").unwrap())
        );
        assert_eq!(
            latest_version(&releases, &Version::parse("0.2.0").unwrap()),
            None
        );
    }

    #[test]
    fn checks_all_release_pages() {
        let other_releases = vec![
            serde_json::json!({"tag_name": "other-v1.0.0", "draft": false, "prerelease": false});
            100
        ];
        let (url, server) = serve_responses(vec![
            serde_json::to_string(&other_releases).unwrap(),
            r#"[{"tag_name":"nddrv-v0.2.0","draft":false,"prerelease":false}]"#.into(),
        ]);
        let result = check_releases(&test_agent(), &url, &Version::parse("0.1.0").unwrap());
        server.join().unwrap();
        assert_eq!(result.unwrap(), Some(Version::parse("0.2.0").unwrap()));
    }

    #[test]
    fn rejects_invalid_release_response() {
        let (url, server) = serve_responses(vec!["not json".into()]);
        let result = check_releases(&test_agent(), &url, &Version::parse("0.1.0").unwrap());
        server.join().unwrap();
        assert!(result.is_err());
    }
}
