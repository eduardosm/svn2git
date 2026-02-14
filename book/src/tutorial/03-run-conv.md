# Run the Conversion

Once everything has been set up, you can launch the conversion with on of the
following commands:

* To use a Subversion repository dump file:

  ```sh
  svn2git -s my-svn-dump.xz -d my-git-repo.git -P my-conv-params.toml --log-file my-conv-log.log
  ```

* To use a local repository mirror:

  ```sh
  svn2git -s /path/to/mirror -d my-git-repo.git -P my-conv-params.toml --log-file my-conv-log.log
  ```

* To use a remote Subversion URL:

  ```
  svn2git -s https://svn/repo/url --remote-svn -d my-git-repo.git -P my-conv-params.toml --log-file my-conv-log.log
  ```

It will create the Git repository at `my-git-repo.git` and a log file at
`my-conv-log.log`.

You can read more about commant line parameters in the
[documentation](../documentation/cli.md).
