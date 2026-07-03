# MVP does not retain historical font versions

Fontbrew's MVP does not provide rollback or retain old installed font versions after a successful update. During an update, the old version remains active until the new version is downloaded, parsed, validated, and activated; after success, the old version is deleted. This keeps the managed store simple and avoids treating downloaded font files as a cache while still protecting users from failed updates.
