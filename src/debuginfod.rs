use crate::Result;

use std::env;
use std::io::Error;
use std::io::ErrorKind;

use reqwest::blocking::{Client, Response};
use reqwest::Method;

#[derive(Debug)]
pub struct DebugInfodResolver {
    client: Client,
    pub upstream_resolvers: Vec<String>,
}

impl DebugInfodResolver {
    pub fn new(client: Client, upstream_resolvers: Vec<String>) -> DebugInfodResolver {
        DebugInfodResolver {
            client: client,
            upstream_resolvers: upstream_resolvers,
        }
    }

    fn do_request(&self, method: Method, path: String) -> Result<Response> {
        if self.upstream_resolvers.len() == 0 {
            return Err(Error::new(ErrorKind::NotFound, "no configured resolvers").into());
        }
        for (resolver, is_last) in self
            .upstream_resolvers
            .iter()
            .enumerate()
            .map(|(i, r)| (r, i == self.upstream_resolvers.len() - 1))
        {
            let builder = self
                .client
                .request(method.clone(), format!("{}{}", resolver, path));
            let res = self.client.execute(builder.build()?);
            match res {
                Ok(res) => {
                    return Ok(res);
                }
                Err(e) => {
                    if !is_last {
                        continue;
                    }
                    return Err(e.into());
                }
            }
        }
        Err(Error::new(ErrorKind::NotFound, "failed to query debuginfo").into())
    }

    pub fn get_debuginfo(&self, build_id: &str) -> Result<Vec<u8>> {
        // /buildid/<BUILDID>/debuginfo
        let mut res = self.do_request(
            Method::from_bytes(b"GET").unwrap(),
            format!("/buildid/{}/debuginfo", build_id),
        )?;
        let mut buf: Vec<u8> = vec![];
        res.copy_to(&mut buf)?;
        Ok(buf)
    }

    pub fn get_executable(&self, build_id: &str) -> Result<Vec<u8>> {
        // /buildid/<BUILDID>/executable
        let mut res = self.do_request(
            Method::from_bytes(b"GET").unwrap(),
            format!("/buildid/{}/executable", build_id),
        )?;
        let mut buf: Vec<u8> = vec![];
        res.copy_to(&mut buf)?;
        Ok(buf)
    }

    pub fn get_source_file(&self, build_id: &str, source: &str) -> Result<String> {
        //  /buildid/BUILDID/source/SOURCE/FILE
        // Examples:
        // #include <stdio.h>	/buildid/BUILDID/source/usr/include/stdio.h
        // /path/to/foo.c	/buildid/BUILDID/source/path/to/foo.c
        let res = self.do_request(
            Method::from_bytes(b"GET").unwrap(),
            format!("/buildid/{}/source/{}", build_id, source),
        )?;
        Ok(res.text()?)
    }

    pub fn get_default_servers() -> Result<Vec<String>> {
        if !env::var("DEBUGINFOD_URLS").is_ok() {
            panic!("DEBUGINFOD_URLS is unset");
        }
        let val = env::var("DEBUGINFOD_URLS");
        let res: Vec<String> = val
            .expect("DEBUGINFOD_URLS misconfigured")
            .split([',', ' '])
            .map(|v| v.to_string())
            .collect();
        Ok(res)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn valid_default_servers_ws() {
        env::set_var(
            "DEBUGINFOD_URLS",
            "https://debug.infod https://de.bug.info.d",
        );
        let resolvers = DebugInfodResolver::get_default_servers().unwrap();
        assert_eq!(resolvers.len(), 2);
        assert_eq!(resolvers[0], "https://debug.infod".to_string());
        assert_eq!(resolvers[1], "https://de.bug.info.d".to_string());
    }
    #[test]
    fn valid_default_servers_comma() {
        env::set_var(
            "DEBUGINFOD_URLS",
            "https://debug.infod,https://de.bug.info.d",
        );
        let resolvers = DebugInfodResolver::get_default_servers().unwrap();
        assert_eq!(resolvers.len(), 2);
        assert_eq!(resolvers[0], "https://debug.infod".to_string());
        assert_eq!(resolvers[1], "https://de.bug.info.d".to_string());
    }
    #[test]
    fn debuginfodresolver_new() {
        env::set_var(
            "DEBUGINFOD_URLS",
            "https://debug.infod,https://de.bug.info.d",
        );
        let resolver = DebugInfodResolver::new(
            Client::new(),
            DebugInfodResolver::get_default_servers().unwrap(),
        );
        assert_eq!(resolver.upstream_resolvers.len(), 2);
    }
}
