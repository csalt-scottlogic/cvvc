use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use cvvc::{
    cli::{branches, init, log, net, objects, ref_log, refs, remotes, staging},
    config::GlobalConfig,
    output::{ConsoleOutputService, OutputKind},
};

fn main() -> ExitCode {
    parse_dispatch()
}

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    /// Print verbose output
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Do not output coloured or highlighted text
    #[arg(long, name = "no-colour", global = true)]
    no_colour: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Add files to the repository
    #[command()]
    Add {
        #[arg(value_name = "PATH")]
        paths: Vec<String>,
    },
    /// List, create or delete branches.  With a branch name given but no other options, this command will create the specified branch.
    /// With no options or arguments, it will behave as if --list was specified.
    #[command()]
    Branch {
        /// List branches; local by default, or all with the --all or -a options.
        #[arg(long)]
        list: bool,
        /// When listing branches, list remote-tracking branches in addition to local ones.
        #[arg(short = 'a', long = "all")]
        list_all: bool,
        /// Delete a branch, if it is fully merged to the current HEAD.
        #[arg(short, long)]
        delete: bool,
        /// Force-delete a branch, whether it is merged or not.
        #[arg(short = 'D', long)]
        force_delete: bool,
        /// The branch to create or delete, unless --list is specified.
        #[arg()]
        branch: Option<String>,
    },
    /// Output the content of a repository object
    #[command(name = "cat-file", arg_required_else_help = true)]
    CatFile {
        /// The object type
        #[arg(value_name = "TYPE")]
        obj_type: String,
        /// The object's ID
        #[arg(value_name = "OBJECT")]
        obj_path: String,
    },
    /// Check path(s) against the ignore rules
    #[command(name = "check-ignore")]
    CheckIgnore {
        #[arg(value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Check if a reference name (or branch name) is valid according to the syntax rules
    #[command(name = "check-ref-format")]
    CheckRefFormat {
        #[arg(value_name = "REFNAME")]
        name: String,
    },
    /// Checkout a commit
    #[command(arg_required_else_help = true)]
    Checkout {
        /// Create a new branch
        #[arg(short = 'b')]
        new_branch: bool,
        /// The commit or tree to check out
        #[arg(value_name = "COMMIT-OR-TREE")]
        target: String,
    },
    /// List all reachable commits, either from all refs or from a given commit.
    #[command(name = "ls-commits")]
    CommitList {
        #[arg(value_name = "COMMIT")]
        starting_commit: Option<String>,
    },
    /// Create a new commit object
    #[command(name = "commit-tree", arg_required_else_help = true)]
    CommitTree {
        /// An existing tree object ID
        #[arg(value_name = "TREE")]
        tree_id: String,
        /// Each -p is the ID of a parent commit
        #[arg(short)]
        parents: Vec<String>,
        /// The commit log message
        #[arg(short, long)]
        message: String,
    },
    /// Record changes to the repository
    #[command()]
    Commit {
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Fetch refs and objects from remote repositories
    #[command()]
    Fetch,
    /// Compute object ID and optionally create an object from a file
    #[command(name = "hash-object")]
    HashObject {
        /// Write the object into the Git store
        #[arg(short)]
        write: bool,
        /// Specify the object type
        #[arg(short = 't', default_value = "blob")]
        obj_type: String,
        /// Object data source
        #[arg(value_name = "FILE")]
        filename: String,
    },
    /// Initialise a new, empty repository
    #[command(arg_required_else_help = true)]
    Init {
        /// Where to create the repository
        #[arg(default_value = ".", value_name = "PATH")]
        pathname: String,
    },
    /// List files in the index
    #[command(name = "ls-files")]
    ListFiles {
        /// Show additional info
        #[arg(short, long)]
        verbose: bool,
    },
    /// Pretty-print a tree object
    #[command(name = "ls-tree")]
    ListTree {
        ///Recurse into sub-trees
        #[arg(short)]
        recursive: bool,
        /// A tree-ish object
        #[arg(value_name = "TREE")]
        tree: String,
    },
    /// Display the history of a given commit
    #[command()]
    Log {
        /// Commit to start at
        #[arg(default_value = "HEAD", value_name = "COMMIT")]
        commit: String,
    },
    /// Examine or edit the reference log
    #[command(name = "reflog")]
    RefLog(RefLogArgs),
    /// View and edit remote information
    #[command()]
    Remote {
        #[arg(short, long)]
        verbose: bool,
    },
    /// Remove files from the index and the working tree
    #[command(name = "rm")]
    Remove {
        #[arg(long = "cached")]
        index_only: bool,
        #[arg(long = "ignore-unmatched")]
        ignore_no_matches: bool,
        #[arg(value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Parse revision and object identifiers
    #[command(name = "rev-parse")]
    RevParse {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// List references
    #[command(name = "show-ref")]
    ShowRef,
    /// Display status of the working tree
    #[command()]
    Status,
    /// List and create tags.  Without any options, lists tags
    #[command()]
    Tag {
        /// Create a chunky tag
        #[arg(short = 'a')]
        chunky: bool,
        /// The message to add to a chunky tag
        #[arg(long, short = 'm')]
        message: Option<String>,
        /// The tag name
        #[arg(value_name = "NAME")]
        name: Option<String>,
        #[arg(default_value = "HEAD", value_name = "OBJECT")]
        target: String,
    },
    /// Output the current branch and last commit
    #[command()]
    Where,
    /// Create a set of tree objects from the current index
    #[command(name = "write-tree")]
    WriteTree {
        #[arg(long = "missing-ok")]
        no_checks: bool,
    },
}

#[derive(Args)]
#[command()]
struct RefLogArgs {
    #[command(subcommand)]
    command: RefLogCommands,
}

#[derive(Subcommand)]
enum RefLogCommands {
    #[command()]
    Exists {
        #[arg()]
        branch: String,
    },
    #[command()]
    List,
    #[command()]
    Show {
        #[arg()]
        branch: Option<String>,
    },
}

fn parse_dispatch() -> ExitCode {
    let args = Cli::parse();
    let config = GlobalConfig::from_default_files();
    let output_kind = if args.no_colour {
        OutputKind::Plain
    } else {
        OutputKind::Colour
    };
    let output_service = ConsoleOutputService::new(output_kind, args.verbose);
    match args.command {
        Commands::Add { paths } => staging::add_files(&paths, &output_service),
        Commands::Branch {
            list,
            list_all,
            branch,
            delete,
            force_delete,
        } => {
            if list {
                branches::list_branches(list_all, &output_service)
            } else if let Some(branch) = branch {
                if delete || force_delete {
                    branches::delete_branch(&branch, force_delete, &output_service)
                } else {
                    branches::new_branch(&branch, false, &config, &output_service)
                }
            } else {
                branches::list_branches(list_all, &output_service)
            }
        }
        Commands::CatFile { obj_type, obj_path } => {
            objects::cat_file(&obj_type, &obj_path, &output_service)
        }
        Commands::CheckIgnore { paths } => staging::check_ignore(&paths, &output_service),
        Commands::CheckRefFormat { name } => {
            if !refs::check_format(&name) {
                return ExitCode::FAILURE;
            }
            Ok(())
        }
        Commands::Checkout { new_branch, target } => {
            if new_branch {
                branches::new_branch(&target, true, &config, &output_service)
            } else {
                branches::checkout(&target, &config, &output_service)
            }
        }
        Commands::Commit { message } => staging::full_commit(&config, message, &output_service),
        Commands::CommitList { starting_commit } => {
            staging::list_commits(starting_commit.as_deref(), &output_service)
        }
        Commands::CommitTree {
            tree_id,
            parents,
            message,
        } => {
            staging::create_commit_for_tree(&tree_id, &parents, &message, &config, &output_service)
        }
        Commands::Fetch => net::fetch(&config, &output_service),
        Commands::HashObject {
            write,
            obj_type: _,
            filename,
        } => objects::object_hash(write, &filename, &output_service),
        Commands::Init { pathname } => {
            init::cmd(&pathname, &config.default_branch_name(), &output_service)
        }
        Commands::ListFiles { verbose } => staging::list_files(verbose, &output_service),
        Commands::ListTree { recursive, tree } => {
            objects::list_tree(recursive, &tree, &output_service)
        }
        Commands::Log { commit } => log::cmd(&commit, &output_service),
        Commands::RefLog(sub_command) => match sub_command.command {
            RefLogCommands::List => ref_log::list(&output_service),
            RefLogCommands::Show { branch } => ref_log::show(branch.as_deref(), &output_service),
            RefLogCommands::Exists { branch } => {
                let exists = ref_log::exists(&branch, &output_service);
                match exists {
                    Ok(true) => Ok(()),
                    Err(x) => Err(x),
                    Ok(false) => {
                        return ExitCode::FAILURE;
                    }
                }
            }
        },
        Commands::Remote { verbose } => remotes::list_remotes(verbose, &output_service),
        Commands::Remove {
            index_only,
            ignore_no_matches,
            paths,
        } => staging::remove_files(&paths, index_only, ignore_no_matches, &output_service),
        Commands::RevParse { name } => objects::rev_parse(&name, &output_service),
        Commands::ShowRef => refs::show_refs(&output_service),
        Commands::Status => staging::status(&output_service),
        Commands::Tag {
            chunky,
            message,
            name,
            target,
        } => match name {
            Some(tag_name) => refs::create_tag(
                &config,
                &tag_name,
                &target,
                chunky,
                message.as_deref(),
                &output_service,
            ),
            None => refs::show_tags(&output_service),
        },
        Commands::Where => staging::current_branch_and_commit(&output_service),
        Commands::WriteTree { no_checks } => {
            staging::store_index_as_tree(no_checks, &output_service)
        }
    }
    .expect("Error!");
    ExitCode::SUCCESS
}
