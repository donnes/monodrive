# Security

Please do not open a public issue for a vulnerability.

[Report it privately](https://github.com/donnes/monodrive/security/advisories/new) through a GitHub security advisory. Include the output of `monodrive --version` or the commit you built, your macOS version, and the steps to reproduce it.

We want to hear about anything that changes or deletes a file nobody asked to change, runs a command through a crafted file name or a remote server's reply, or lets another local user reach Monodrive's owner process.

An agent connected over MCP can search your disk and change files, just as you can in the window. That access is by design, so give it only to agents you trust.
