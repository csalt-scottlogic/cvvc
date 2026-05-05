# CVVC Functionality Level

## High-level summary

At present CVVC can process most parts of my current workflow that solely remain on the local machine.  It can *not* handle communication with remotes, merging, rebasing, or repacking and/or garbage collection.

## Versioning

CVVC supports the following file versions:

- Repository version 2 (ie, with no extensions)
- Disk index version 2
- Pack version 2
- Pack index version 2
- Pack reverse index version 1

## Features common to all commands

Commands referring to a commit accept the following arguments:

- Full object IDs
- Abbreviated object IDs (an initial substring of a full object ID) if unambiguous
- Tag names
- Branch names
- Remote branch names
- the special term `HEAD`

They do *not* accept:

- `git describe` output
- remote `HEAD`s
- special terms ending in `_HEAD`, such as `FETCH_HEAD` or `ORIG_HEAD`
- the special term `AUTO_MERGE`
- Revision names with a date specification
- Revision names with an index-from-tip suffix
- Revision names with an index-from-current suffix
- Branch names with a suffix indicating the remote branch they are tracking, either for pulling or pushing
- Revision names with a suffix indicating the numbered parent of a commit
- Revision names with a suffix indicating an object type
- Tag names with a suffix indicating that the tag should be dereferenced
- Suffixes which search for commits with messages matching a regex
- A suffix which specifies a blob by filename, with or without a stage number
- Revision ranges, with or without exclusions

The git disambiguation rules have not been fully checked to confirm they are applied.

Commands do not accept the `--color` or `--no-color` options for colouring the output.

CVVC supports reading loose objects and objects in packfiles.  If it encounters a packfile with a missing index, it will silently reindex it, unless the packfile contains objects consisting of a diff against an object that is referred to by its ID---in CVVC this is referred to as a "named delta" object.  It can write reverse index files for packs, but does not read them.  It cannot write packfiles.

## Support for individual commands

At present `cv` accepts the following command verbs, with limitations as described:

### `cv add`

The `add` command takes one or more paths as positional parameters, but does not take any options.  It is intended that in future, `cv add` with no parameters will be the equivalent of `git add -A`, but this remains to be implemented.

### `cv branch`

The `branch` command has the following forms:

- `cv branch`
- `cv branch --list`
- `cv branch <new-branch>`

The first two both list extant branches; the third creates a new branch that is not checked out.

### `cv cat-file`

This command accepts a `-t` option to confirm the type of the supplied object, but this is not required.

### `cv check-ignore`

This command takes no options (including `--stdin`) but otherwise behaves as `git check-ignore`.

### `cv checkout`

This command is two forms:

- `cv checkout -b <new-branch>`, which creates a branch based on the current commit and checks it out
- `cv checkout REV [<dir>]`, which checks out an existing revision, potentially into the specified directory

At present, `cv checkout REV` without a directory, when run in a subdir of a working directory, does not give the expected result (issue #43).

### `cv commit-tree`

The `cv commit-tree` command accepts `-p` and `-m` options only.

### `cv commit`

This command only accepts the `-m` option.

### `cv hash-object`

This command accepts `-w` and `-t` options only.

### `cv init`

This command takes the form `cv init [<path>]`, with `path` defaulting to the working directory.  It does not accept any options.

This command creates an initial `main` branch, and does not honour the `init.defaultbranch` setting (see issue #28).

### `cv ls-files`

This command only lists files in the index, and takes a `--verbose` or `-v` option which lists all fields present in the index for each file, similar to the `-v` option in `wyag ls-files`.  It takes no other options.

### `cv ls-tree`

The `cv ls-tree` command accepts an `-r` or `--recursive` option, and no others.

### `cv log`

The `cv log` command takes an optional revision to log back from, and no others.  The revision argument defaults to `HEAD`.

At present this command produces GraphViz output, as per `wyag`.

### `cv reflog`

This command has multiple subcommands.

- `cv reflog list` displays a list of the ref-logs that are present in the repository
- `cv reflog show <branch>` displays the contents of the listed ref-log
- `cv reflog exists <branch>` returns successfully if the given branch has a ref-log, and errors if it does not.

None of the subcommands take any other options.

### `cv show-ref`

This command takes no options.  It has a known issue whereby it does not list all of the refs that `git show-ref` does (see issue #44).

### `cv status`

This command takes no options.

### `cv tag`

This command supports the `-a` and `-m` options.  Like  `git tag`, when run with no arguments it displays all tags in the repository.

### `cv write-tree`

This command supports the `--missing-ok` option, but not the `--prefix` option.
