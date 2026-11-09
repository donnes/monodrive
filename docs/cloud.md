# Cloud folders

iCloud Drive and File Provider apps such as Dropbox, Google Drive and OneDrive keep some files only in the cloud. Each such file is a placeholder: the kernel marks it `SF_DATALESS`, and `lstat` still gives its name, size and dates. Reading a placeholder makes macOS download it before the read returns, and listing a placeholder folder makes the provider fetch its items. `monodrive-core`'s `cloud` module decides what counts as a placeholder, and everything that walks or reads the disk checks it first.

## Finding the folders

`cloud::folders` lists iCloud Drive and each folder in `~/Library/CloudStorage`, for the sidebar's Locations, the palette's Cloud storage page and `list_places`. It uses one `stat` of `Mobile Documents/com~apple~CloudDocs` and one listing of `CloudStorage`, with the type from the directory entry. It never goes inside a provider's folder, since that can ask for permission or wake the provider. Each folder goes by the name Finder gives it, such as "Proton Drive". No public API says what that name is, so it comes from the private `FPProviderDomain`, which asks fileproviderd and takes about 3 ms a folder. Without that answer, it falls back to macOS's `Provider-account` folder name, so `GoogleDrive-me@example.com` goes by `GoogleDrive`. Two folders that end up with the same name each go by their whole folder name. What it can't see isn't listed.

## Availability

`cloud::Availability` comes from the `st_flags` of the `lstat` a listing already makes, so it adds no system call. A listing says `Local` or `Placeholder`. Only `info` asks Foundation whether a placeholder is `Downloading`, whether the item is in the cloud at all, and why its last download failed. Those questions go to the provider's daemon, which takes about a millisecond an item. A search result from the index is `Unknown`, because fsearch keeps no flags.

## Folders that aren't all downloaded

Finder badges a folder when anything under it, at any depth, is a placeholder, even though the folder itself is on disk. fileproviderd keeps that answer as each item's `isRecursivelyDownloaded`, and Foundation has no key for it: `NSURLUbiquitousItemDownloadingStatusKey` only says whether the folder itself is downloaded. `cloud::fully_downloaded` asks the private `FPItemManager`, as Finder does, about 1.5 ms an item.

We chose the private class over keeping our own copy of that answer, which would mean walking every cloud folder and watching it for downloads and evictions. A macOS without the class answers `None`, so folders lose their badge and nothing else changes; files keep theirs, since it comes from their flag. `cloud::partly_downloaded` asks only about the folders in a listing whose parent is in a File Provider domain, so listing a folder elsewhere costs one quick `None`. The pane asks after a listing lands and badges folders when the answer arrives, and asks again whenever it lists the folder again. `list_dir`, `stat` and the inspector use the same answer.

## What never downloads

- Listing a folder never reads its files. Opening a placeholder folder lists it, as Finder does, because there's no other way to show what it holds.
- The walk shared by folder sizes and storage scans counts a placeholder file at its size, on no blocks, and doesn't list a placeholder folder. Totals report how many placeholders they hold. A placeholder folder given as the folder to walk is refused.
- Looking for duplicates never reads a placeholder. One the size of another file is listed as not downloaded, neither verified nor ruled out.
- Listing an archive's contents refuses a placeholder, following links as opening does.
- The duplicate reader sets fsearch's `no_materialize` policy on its threads, and the archive reader sets it for the read with `cloud::without_downloads`. A file evicted after the check then fails to read, and says it isn't downloaded, instead of downloading.
- Operation stamps and tallies treat a placeholder folder as empty, so trashing or moving one on its volume fetches nothing.

## Copies refuse placeholders

Copy, a move to another volume, Duplicate, Compress and Extract read contents, so the operation owner refuses any item that is a placeholder or holds one. The refusal names the path and says to download it first. A move within one volume is a rename and moves placeholders as they are. Downloading inside a job would make the job wait on the network with no progress to show. A separate, explicit download keeps that work visible and lets the job continue afterwards. Quick Look leaves downloading to macOS, as Finder does.

## Download and remove download

`cloud::download` asks `NSFileManager.startDownloadingUbiquitousItem(at:)`. On current macOS this covers iCloud Drive and File Provider domains alike: their items report the ubiquitous resource keys. It returns once macOS accepts the request; `info` then shows `Downloading` and, when the download fails, the provider's reason. An item outside iCloud Drive and File Provider folders is refused. A placeholder whose provider doesn't expose these keys gets "downloading is unsupported for this provider".

`cloud::remove_download` is Finder's Remove Download, through `evictUbiquitousItem(at:)`. It refuses an item that isn't uploaded yet or has unresolved conflicts, because the local copy may be the only one. Downloading again reverses it. Agents can do either only while the Agents setting lets them change files; the MCP server checks that before asking.

## Downloads in the window

macOS doesn't say when a download ends, so the app's `downloads` list checks each one it asked for with `cloud::progress` once a second while any are on their way, and not at all otherwise. A download is done when the item, and for a folder everything under it, is no longer a placeholder; it failed when the provider reports an error, which is how an interrupted download shows up, or when the item can't be read any more. Every pane, the inspector, the preview and the jobs popover read that one list, and whatever lists the item lists it again when it's done.

The badge is the way in: clicking it downloads, it turns into a still downloading glyph, and a failure turns it red and tries again on a click. Download and Remove Download are commands, offered only where they run. Opening a placeholder downloads it first and opens it in its app once it's here, so the app never sits waiting on the network. The preview never downloads on its own, so arrowing through a cloud folder fetches nothing; it offers Download. A copy refused for a placeholder offers Download in its toast or in the jobs popover, where the job runs again once it lands. Remove Download's toast offers Download as the way back.

There is no progress fraction: Foundation doesn't expose one for File Provider items that we can check without a real provider, and nothing in the window repaints continuously.
