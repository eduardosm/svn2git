# Configure the Conversion

Before running **svn2git**, you need to create a configuration file to set some
parameters of the conversion.

Create a file that maps Subversion user names to Git names/emails (let's name
it `my-user-map.txt`):

```text
user1 = User One <user1@somewhere>
user2 = User Two <user2@somewhere>
```

And a TOML file with some configuration parameters (let's name it
`my-conv-params.toml`):

```toml
# Specify which directories within the repository are considered branches
branches = [
  # "trunk" is a branch
  "trunk",
  # Each directory inside "branches" is a branch
  "branches/*",
  # "branches/more_branches" is not a branch itself.
  # Instead, each subdirectory is a branch
  "branches/more_branches/*",
]
# Rename "trunk" branch to "master"
rename-branches."trunk" = "master"
# Remove "branches/" prefix from all branches in "branches/"
rename-branches."branches/*" = "*"

# Specify which directories within the repository are considered tags
tags = [
  # Each directory inside "tags" is a tag
  "tags/*",
]
# Remove "tags/" prefix from all tags in "tags/"
rename-tags."tags/*" = "*"

# Specify the Subversion branch whose converted Git branch will become the Git
# HEAD.
head = "trunk"

# Specify the name of the Git branch where anything that is not part of a
# Subversion branch or tag will be placed
unbranched-name = "unbranched"

# Specify the file that maps Subversion users to Git names/emails.
user-map-file = "my-user-map.txt"
```

You can find more conversion parameters and their description in the
[documentation](../documentation/conv-params.md).
