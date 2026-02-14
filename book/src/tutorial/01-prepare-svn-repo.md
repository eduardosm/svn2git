# Prepare the Subversion Repository

**svn2git** needs a Subversion repository dump. It can read that dump in
different ways:

* By reading the dump from a file, which can be optionally compressed.
* By invoking `svnadmin dump` on a local mirror and consuming its output on the
  fly.
* By invoking `svnrdump dump` on a remote repository and consuming its output
  on the fly.

The third method is the easiest one, since you just need to pass the repository
URL to `svn2git` (with the `--remote-svn` option). However, it is not recommended
for large repositories, since a network issue can interrupt the conversion,
requiring to start the conversion from the beginning. Also, if you want to run
multiple conversion attempts with different options, reading the remote repository
every time can become a bottleneck. In those cases, it is better to create a
local mirror.

A mirror can be created with the `svnsync` tool for any repository to which you
have at least read-only access.

1. Choose a path where the mirror will be stored (we will use `/path/to/mirror`
   in this tutorial) and initialize an empty Subversion repository:

   ```sh
   svnadmin create /path/to/mirror
   ```

2. Create a `pre-revprop-change` hook that does nothing.

   On Linux, create `/path/to/mirror/hooks/pre-revprop-change` with the
   following content:

   ```sh
   #!/bin/sh
   exit 0
   ```

   And give it execution permissions:

   ```sh
   chmod +x /path/to/mirror/hooks/pre-revprop-change
   ```

   On Windows, create `/path/to/mirror/hooks/pre-revprop-change.bat` with the
   following content:

   ```bat
   exit 0
   ```

3. Set up the new repository to be a mirror of the desired repository:

   ```sh
   svnsync init file:///path/to/mirror https://url/to/remote/repository
   ```

   Note that `svnsync` needs the mirror path to be absolute and prefixed with
   `file://`.

4. Perform the mirroring:

   ```sh
   svnsync sync file:///path/to/mirror
   ```

   This can take some time depending on the size of the repository. You can
   stop it at any time with <kbd>Ctrl</kbd>+<kbd>C</kbd> and resume it by
   running the command again. If new revisions are committed to the original
   repository after the process has finished, you can run the above command
   again to get them into your mirror.

Once mirroring is finished, you can pass `/path/to/mirror` to **svn2git**.

Alternatively, you can generate a dump file with `svnadmin dump`. It is
recommended to pass `--deltas` and compress the output, which can
significantly reduce the resulting file size:

```sh
svnadmin dump --deltas /path/to/mirror | xz -z -c > my-svn-dump.xz
```
