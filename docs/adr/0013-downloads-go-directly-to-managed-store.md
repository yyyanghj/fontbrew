# Downloads go directly to managed store

Fontbrew does not maintain a separate download cache for font archives or provider assets. Install operations download directly into the managed package store, and removing a package deletes its managed store files and activation artifacts. Fontbrew may keep provider metadata snapshots, but downloaded font files are package state rather than reusable cache entries.
