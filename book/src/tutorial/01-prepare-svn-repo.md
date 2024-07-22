# Prepare the Subversion Repository

**svn2git** cannot convert a repository directly with an URL or a working copy.
It needs a Subversion repository dump.

If you have access to the machine where the repository is hosted, you can use
the `svnadmin dump` command to generate a dump. It is recommended to pass the
`--deltas` option and compress the output, which will significantly reduce the
size of the resulting file.

For example, you can use the following command to generate a deltified dump and
compress it with XZ:

```sh
svnadmin dump --deltas /path/to/repository | xz -z -c > my-svn-dump.xz
```

If you do not have access to the machine where the repository is hosted, you
will need to create a mirror of the repository using the `svnsync` tool. This
can be done for any repository you have read-only access.

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

Once mirroring has finished, you can generate a repository dump:

```sh
svnadmin dump --deltas /path/to/mirror | xz -z -c > my-svn-dump.xz
```
