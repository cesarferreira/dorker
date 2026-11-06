# Disk and cleanup

Every Dorkerfile change makes a new image of about 3–5 GB. `dorker ls` shows what you have. `dorker prune` removes what you no longer need.

## See what is using disk

```bash
dorker ls  # images, instances and storage (alias: df)
dorker ls --json
```

It lists every image (tags, Android version, size, last used, how many instances use it), every instance (with its own size), and a storage summary: totals, how much `dorker prune` could reclaim, free space on the disk, and the Android SDK system images dorker does not manage.

On macOS, `dorker ls` reports the space deleting an instance would free, which for an APFS clone is only what that instance wrote. A Linux reflink (XFS, Btrfs) shares blocks the same way, but `ls` cannot see that, so it counts the full allocated size and overstates the cost. On NFS and ext4 the copy really is full, and the number is exact.

Put the store on a local filesystem that can clone files. See [Images and instances](../concepts/images-and-instances.md#copies-and-disk).

## Free disk space

```bash
dorker prune --dry-run           # show what would be removed
dorker prune                     # remove it, after asking
dorker prune --all --instances   # also tagged images and stopped instances
dorker prune --older-than 7d -y  # only things idle for a week, no prompt
```

By default it removes only images with no tag that no instance was made from. Tagged images, images behind existing instances, and running instances are always kept. `--all` adds unused tagged images. `--instances` adds stopped ones. It lists what it will remove first and, outside a terminal, refuses to delete without `--yes`. It also clears the leftovers of interrupted builds. A removed image is simply rebuilt the next time you need it.

## Stop everything

```bash
dorker stop --all      # every running instance
dorker stop --orphans  # emulators dorker lost track of
```

`dorker ps` warns about an orphan. `--orphans` asks before stopping, because such an emulator may belong to another `DORKER_HOME` that is still using it. `--yes` skips the question.

Next: [Configuration →](../configuration/index.md)
