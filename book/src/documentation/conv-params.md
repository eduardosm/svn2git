# Conversion Parameters

* `branches` and `tags` (default: empty array)

  Arrays that specify which directories should be considered branches and tags.
  `*` can be used as a wildcard to specify directories where each subdirectory
  is a branch or tag.

  <u>Example</u>

  ```toml
  branches = [
    # Consider "trunk" a branch.
    "trunk",
    # Consider each subdirectory in "branches" a branch.
    "branches/*",
    # Do not consider "branches/more" a branch. Instead, consider each
    # subdirectory a branch.
    "branches/more/*",
  ]

  tags = [
    # Consider each subdirectory in "tags" a tag.
    "tags/*",
  ]
  ```

* `rename-branches` and `rename-tags` (default: empty table)

  By default, branches and tags will have the SVN path as the name. These
  options allow specifying key-value maps to rename branches and tags.

  There can be exact renames and prefix replacement renames.

  <u>Example</u>

  ```toml
  # Rename "trunk" to "master" (exact rename).
  rename-branches.trunk = "master"
  # Rename "branches/<name>" to "b-<name>" (prefix replacement).
  rename-branches."branches/*" = "b-*"

  # Rename "tags/<name>" to "<name>" (prefix replacement).
  rename-tags."tags/*" = "*"
  ```

* `keep-deleted-branches` and `keep-deleted-tags` (default: `true`)

  Specifies whether the Git repository should keep branches or tags that have
  been removed from the Subversion repository.

  <u>Example</u>

  ```toml
  keep-deleted-branches = false
  keep-deleted-tags = false
  ```

* `partial-branches` and `partial-tags` (default: empty array)

  Arrays that specify which branches or tags are allowed to be created as
  partial branches/tags. Partial branches are created from subdirectories
  (e.g., `<BRANCH_DIR>/subpath` instead of `<BRANCH_DIR>`). The converted Git
  branches will contain the complete branch tree structure.

  This is useful when SVN branches are created by copying only a subdirectory
  of another branch instead of the entire branch.

  **Note:** This feature is experimental.

  <u>Example</u>

  ```toml
  branches = [
    "branches/*",
    "branches/more/*"
  ]
  partial-branches = [
    # Allow branch "branches/some_branch" to be a partial branch
    "branches/some_branch",
    # Allow each branch in "branches/more" to be a partial branch
    "branches/more/*",
  ]

  partial-tags = [
    # Allow each tag in "tags" to be a partial tag
    "tags/*",
  ]
  ```

* `head` (default: `trunk`)

  Specifies which branch will be used as Git HEAD. You have to specify the
  Subversion path of the branch (even if you renamed it with
  `rename-branches`).

  <u>Example</u>

  ```toml
  # The branch whose SVN path is "trunk" will be used as Git HEAD.
  head = "trunk"
  ```

  You can set it to an empty string to use the unbranched branch (see below) as
  HEAD.

  <u>Example</u>

  ```toml
  head = ""
  ```

* `unbranched-name`

  Specifies the name of the Git branch where everything that is not part of a
  branch or a tag (as specified with `branches` or `tags`) will be placed. If
  not specified, these files will be discarded.

  <u>Example</u>

  ```toml
  unbranched-name = "unbranched"
  ```

* `enable-merges` (default: `true`)

  Whether to enable or not the generation of Git merges based on Subversion
  mergeinfo.

  <u>Example</u>

  ```toml
  # Disable Git merges
  enable-merges = "false"
  ```

* `generate-gitignore` (default: `true`)

  Whether to generate `.gitignore` files from `svn:ignore` and
  `svn:global-ignores` properties. Existing `.gitignore` files in the
  Subversion repository will be removed or replaced.

  <u>Example</u>

  ```toml
  # Generate .gitignore files
  generate-gitignore = "true"
  ```

* `delete-files` (default: empty array)

  Array of patterns that match names of files that should be deleted.

  <u>Example</u>

  ```toml
  delete-files = [
    # Delete ".cvsignore" files
    ".cvsignore",
    # Delete vim temporary files (.swo, .swp, .swn, ...)
    "*.sw?",
  ]
  ```

* `user-map-file`

  Specifies the path (relative to the location of the parameters TOML file)
  which maps Subversion usernames to Git names/emails.

  <u>Example</u>

  ```toml
  user-map-file = "user-map.txt"
  ```
