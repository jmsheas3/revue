# Security policy

## Reporting a vulnerability

Please don't open a public issue. Report it privately through GitHub instead: on the repo's **Security** tab, choose **Report a vulnerability** ([direct link](https://github.com/jmsheas3/revue/security/advisories/new)). Only you and the maintainer can see the report.

It helps to include:

- what is affected: an API endpoint, a view of the page, an MCP tool, the command line;
- how to reproduce it, ideally as a small repo or file;
- the output of `revue --version`;
- your OS and browser, with their versions.

revue is maintained by one person in their spare time, so there's no fixed response time, but reports are read and answered as soon as possible.

## Supported versions

revue has no releases yet. Only the latest commit on `main` is supported, and fixes land there. To update, run the [install command](README.md#install) again. Then restart any running servers: until you do, the page shows **Server out of date**.

revue is tested on Linux and macOS. The Windows code compiles but hasn't been run yet, so a Windows report may take longer to confirm.
