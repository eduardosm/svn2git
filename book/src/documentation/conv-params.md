# Conversion Parameters

* `branches` and `tags` (default: empty array)

  Arrays that specify which directories should be considered branches and tags.
  `*` can be used as a wildcard to specify directories where each subdirectory
  is a branch or tag.

  <u>Example</u>

  ```yaml
  branches:
    # Consider "trunk" a branch.
    - trunk
    # Consider each subdirectory in "branches" a branch.
    - branches/*
    # Do not consider "branches/more" a branch. Instead, consider each
    # subdirectory a branch.
    - branches/more/*

  tags:
    # Consider each subdirectory in "tags" a tag.
    - tags/*
  ```

* `rename-branches` and `rename-tags` (default: empty array)

  By default, branches and tags will have the SVN path as the name. These
  options allow specifying key-value maps to rename branches and tags.

  There can be exact renames and prefix replacement renames.

  <u>Example</u>

  ```yaml
  rename-branches:
    # Rename "trunk" to "master" (exact rename).
    trunk: master
    # Rename "branches/<name>" to "b-<name>" (prefix replacement).
    branches/*: b-*

  rename-tags:
    # Rename "tags/<name>" to "<name>" (prefix replacement).
    tags/*: "*"
  ```

* `keep-deleted-branches` and `keep-deleted-tags` (default: `true`)

  Specifies whether the Git repository should keep branches or tags that have
  been removed from the Subversion repository.

  <u>Example</u>

  ```yaml
  keep-deleted-branches: false
  keep-deleted-tags: false
  ```

* `head` (default: `trunk`)

  Specifies which branch will be used as Git HEAD. You have to specify the
  Subversion path of the branch (even if you renamed it with
  `rename-branches`).

  <u>Example</u>

  ```yaml
  # The branch whose SVN path is "trunk" will be used as Git HEAD.
  head: trunk
  ```

  You can set it to an empty string to use the unbranched branch (see below) as
  HEAD.

  <u>Example</u>

  ```yaml
  head: ""
  ```

* `unbranched-name` (default: `unbranched`)

  Specifies the name of the Git branch where everything that is not part of a
  branch or a tag (as specified with `branches` or `tags`) will be placed.

  <u>Example</u>

  ```yaml
  unbranched-name: unbranched
  ```

* `enable-merges` (default: `true`)

  Whether to enable or not the generation of Git merges based on Subversion
  mergeinfo.

  <u>Example</u>

  ```yaml
  # Disable Git merges
  enable-merges: false
  ```

* `generate-gitignore` (default: `true`)

  Whether to generate `.gitignore` files from `svn:ignore` and
  `svn:global-ignores` properties. Existing `.gitignore` files in the
  Subversion repository will be removed or replaced.

  <u>Example</u>

  ```yaml
  # Generate .gitignore files
  generate-gitignore: true
  ```

* `delete-files` (default: empty array)

  Array of regular patterns that match paths of files that should be deleted.

  <u>Example</u>

  ```yaml
  delete-files:
    # Delete ".cvsignore" files (regardless of their location)
    - "**/.cvsignore"
  ```

* `user-map-file`

  Specifies the path (relative to the location of the parameters YAML file)
  which maps Subversion usernames to Git names/emails.

  <u>Example</u>

  ```yaml
  user-map-file: user-map.txt
  ```
