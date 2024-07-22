# Run the Conversion

Once everything has been set up, you can launch the conversion with the
following command (assuming you placed the dump and configuration files in the
same directory):

```sh
svn2git -s my-svn-dump.xz -d my-git-repo.git -P my-conv-params.yaml --log-file my-conv-log.log
```

It will create the Git repository at `my-git-repo.git` and a log file at
`my-conv-log.log`.

You can read more about commant line parameters in the
[documentation](../documentation/cli.md).
