# Security policy

For what revue reads and writes, who can reach it and how it treats repo content, see [docs/security.md](docs/security.md).

## Reporting a vulnerability

Please don't open a public issue. Report it privately through GitHub instead: on the repo's **Security** tab, choose **Report a vulnerability** ([direct link](https://github.com/jmsheas3/revue/security/advisories/new)). Only you and the maintainer can see the report.

It helps to include:

- what is affected: an API endpoint, a view of the page, an MCP tool, the command line;
- how to reproduce it, ideally as a small repo or file;
- the output of `revue --version`;
- your OS and browser, with their versions.

revue is maintained by one person in their spare time, so there's no fixed response time, but reports are read and answered as soon as possible.

## Supported versions

Only the [latest release](https://github.com/jmsheas3/revue/releases/latest) is supported; fixes land on `main` and ship in the next release. To update, [install](README.md#-install) it again. Then restart any running servers: until you do, the page shows **Server out of date**.

revue is tested on Linux and macOS. Windows support is experimental, so a Windows report may take longer to confirm.
