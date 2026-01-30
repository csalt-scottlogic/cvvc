use clap::{Parser, Subcommand};

use crate::shared::config::GlobalConfig;

mod checkout;
mod init;
mod log;
mod objects;
mod refs;
mod shared;
mod staging;

#[derive(Parser)]
#[command(version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Add files to the repository
    #[command()]
    Add {
        #[arg(value_name = "PATH")]
        paths: Vec<String>,
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
    /// Checkout a commit
    #[command(arg_required_else_help = true)]
    Checkout {
        /// The commit or tree to check out
        #[arg(value_name = "COMMIT-OR-TREE")]
        obj: String,
        /// The directory to check out into
        #[arg(value_name = "DIR", default_value = ".")]
        path: String,
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
        /// The tag name
        #[arg(value_name = "NAME")]
        name: Option<String>,
        #[arg(default_value = "HEAD", value_name = "OBJECT")]
        target: String,
    },
    /// Create a set of tree objects from the current index
    #[command(name = "write-tree")]
    WriteTree {
        #[arg(long = "missing-ok")]
        no_checks: bool,
    },
}

pub fn parse_dispatch() {
    let args = Cli::parse();
    let config = GlobalConfig::from_default_files();
    match args.command {
        Commands::Add { paths } => staging::add_files(&paths),
        Commands::CatFile { obj_type, obj_path } => objects::cat_file(&obj_type, &obj_path),
        Commands::CheckIgnore { paths } => staging::check_ignore(&paths),
        Commands::Checkout { obj, path } => checkout::checkout(&obj, &path),
        Commands::Commit { message } => staging::full_commit(&config, message),
        Commands::CommitTree {
            tree_id,
            parents,
            message,
        } => staging::create_commit_for_tree(&tree_id, &parents, &message, &config),
        Commands::HashObject {
            write,
            obj_type,
            filename,
        } => objects::object_hash(write, &obj_type, &filename),
        Commands::Init { pathname } => init::cmd(&pathname),
        Commands::ListFiles { verbose } => staging::list_files(verbose),
        Commands::ListTree { recursive, tree } => objects::list_tree(recursive, &tree),
        Commands::Log { commit } => Ok(log::cmd(&commit)),
        Commands::Remove {
            index_only,
            ignore_no_matches,
            paths,
        } => staging::remove_files(&paths, index_only, ignore_no_matches),
        Commands::RevParse { name } => objects::rev_parse(&name),
        Commands::ShowRef => refs::show_refs(),
        Commands::Status => staging::status(),
        Commands::Tag {
            chunky,
            name,
            target,
        } => match name {
            Some(tag_name) => refs::create_tag(&tag_name, &target, chunky),
            None => refs::show_tags(),
        },
        Commands::WriteTree { no_checks } => staging::write_index(no_checks),
    }
    .expect("Error!")
}
