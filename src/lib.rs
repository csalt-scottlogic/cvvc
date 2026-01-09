use clap::{Parser, Subcommand};

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
}

pub fn parse_dispatch() {
    let args = Cli::parse();
    match args.command {
        Commands::CatFile { obj_type, obj_path } => objects::cat_file(&obj_type, &obj_path),
        Commands::CheckIgnore { paths } => staging::check_ignore(&paths),
        Commands::Checkout { obj, path } => checkout::checkout(&obj, &path),
        Commands::HashObject {
            write,
            obj_type,
            filename,
        } => objects::object_hash(write, &obj_type, &filename),
        Commands::Init { pathname } => init::cmd(&pathname),
        Commands::ListFiles { verbose } => staging::list_files(verbose),
        Commands::ListTree { recursive, tree } => objects::list_tree(recursive, &tree),
        Commands::Log { commit } => Ok(log::cmd(&commit)),
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
    }
    .expect("Error!")
}
