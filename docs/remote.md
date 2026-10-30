# Remote sources

A remote source is a saved SSH server that Monodrive browses and copies to and from over SFTP. Nothing is installed on the server. `remote` holds sources, connections and browsing; `ops/transfers.rs` holds the copies. The window and the MCP tools share both.

## Connecting

A connection is `/usr/bin/ssh -s -- host sftp` with SFTP over its stdio, and no shell. The user's ssh config, agent, known_hosts and ProxyJump all apply, and Monodrive holds no keys. `ssh::arguments` passes what sftp(1) passes, so a host's config can't forward the agent or run a local command, and adds `BatchMode=yes`. Nothing prompts, so authentication that needs a password fails and reports "interactive authentication required". A window prompt is a later decision. `StrictHostKeyChecking=yes` is always passed, whatever the user's config says, because with it off ssh connects through a changed key and says so only on stderr. So an unknown host is refused too, and the error says to connect once with ssh in a terminal so it's in known_hosts. Monodrive offers no way past a host key.

A host is checked when it's saved so that it can't read as an option or carry anything for a shell. `ssh::diagnose` turns what ssh says on stderr into a typed error: host key changed, host key unknown, interactive authentication, authentication failed, unreachable, or no SFTP.

Each process has at most one connection per source, opened on demand. Connecting only agrees on the protocol, and nothing is scanned. A connection allows 64 requests on their way, and ends after 20 seconds without a protocol agreement or 60 seconds without a reply to a request. Disconnecting kills the subprocess by the handle it was started with. Forgetting a source disconnects it and changes nothing on the server. The window, each MCP server and the owner connect separately: the owner's connections last only while a job needs them, so an agent disconnecting doesn't stop a copy.

## The protocol

`remote/sftp.rs` is a client for the part of SFTP 3 Monodrive uses, over any `Read` and `Write` pair. It's hand-written because ssh is the transport and Monodrive needs only a small subset. A reader thread hands replies to the requests waiting for them, and a writer thread sends, so a hung server fails a request by its timeout instead of blocking the caller.

What a server sends is untrusted. A frame over 256 KB ends the connection, and so does any reply that's cut short, of the wrong kind, or holds more data than was asked for. A name from a listing is used only if it's UTF-8 and a single plain name. `..`, a `/` or a NUL can't reach a path, so a listing can't direct a write outside the copy. The rest are left out and counted, which means a file with a non-UTF-8 name can't be addressed yet.

## Paths

A `RemotePath` is a source id and an absolute path with no `..`, and it doesn't convert to a local path. Printed as `source:/path` it isn't absolute, so every local service refuses it, and a control character in a name prints escaped, so one line is always one path. A `Connection` refuses a path on another source.

## What a source can do

List, read, write, make folders, rename and show metadata. It has no Trash, and Monodrive offers no remote delete, since SFTP's unlink can't be undone and isn't one. Rename never replaces. For the same reason a remote move isn't offered, only a copy.

## Copies

A copy is an owner job (`Request::Download`, `Request::Upload`), so it has the same queue, journal, progress, Cancel, Continue and job tools as any other. The owner's request loop answers one at a time, so nothing there touches the network: a job finds out about its source on its own thread, and a bad source fails the job, not the request. A job's `into` and `Task::source` print `source:/path` labels for the server's side. `Job::transfer` says which way a job goes.

A download makes its copy the way any local copy does: a hidden name beside the destination, then a rename into place. Conflicts act on the local side, Replace sends what's there to the Trash, a partial copy goes to the Trash, and Undo trashes the copy.

An upload writes to `.monodrive-{job}-{item}-{random}` beside the destination. The name goes in the journal before the file is created with `O_EXCL`, and a second record says the server confirmed creating it. Only then is it the job's. A failed create removes nothing, and a temp the job can't prove it made, after a connection lost during the create or a crash before the second record, stays on the server and is reported once. Only `Connection::discard` removes anything from a server, and it refuses a name that isn't a temp's. Keep Both takes the next free numbered name, and Skip and Stop act as they do locally. Replace is for good, since there's no Trash: only a file takes a file's place, in one step through `posix-rename@openssh.com`, and only if the file there hasn't changed since the item started. A server without that extension refuses Replace. Nothing can undo an upload, and job status says so.

Each file's size is checked after the copy, and its size and modification time must match before and after. Links stay links. Sockets, pipes and devices and names that can't be addressed don't copy, and the item reports them as a failure.

## Lost connections

A connection that's lost, or can't be made, interrupts the job. The item it was on fails with the reason and the rest stay pending, rather than each failing the same way. Nothing resumes partway. Continue connects again, removes the temp the last attempt is known to have made, checks that the source is as it was when the item first started and that the destination is still a folder, applies the conflict choice again, and starts the item over. A disconnected upload's temp stays on the server until the job is continued, and the job's message says where it is, because only a connection can remove it.

## Previews

`Previews` downloads one file to a real, read-only local path under `~/Library/Application Support/Monodrive/previews`, for Quick Look or Open With. A `Preview` removes its file when dropped, and what doesn't fit in the cache's 1 GB is refused. A file that has grown since the size reserved for it is refused before anything is written, and reads stop at that size. A flock marks each cache as alive, and the next process to open a cache removes those whose lock is free, so a crash leaves nothing for long. Writing a change back to the server is deferred.

## Verifying

Tests run `/usr/libexec/sftp-server` directly over pipes, with no ssh, network or keys, and a hand-written server for malformed replies. A stopped connection is the test killing the server it spawned, after a set number of bytes. `examples/remote_e2e.rs` proves the real ssh path against a throwaway sshd on loopback, with its config, keys and known_hosts in a temp folder under `target/`, and refuses a changed host key. Nothing ever connects to a real machine.
