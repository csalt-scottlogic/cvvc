use clap::{Parser, Subcommand};

mod init;
mod log;
mod objects;
mod shared;

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
    /// Display the history of a given commit
    #[command()]
    Log {
        /// Commit to start at
        #[arg(default_value = "HEAD", value_name = "COMMIT")]
        commit: String,
    }
}

pub fn parse_dispatch() {
    let args = Cli::parse();
    match args.command {
        Commands::CatFile { obj_type, obj_path } => objects::cat_file(&obj_type, &obj_path),

        Commands::HashObject {
            write,
            obj_type,
            filename,
        } => objects::object_hash(write, &obj_type, &filename),
        Commands::Init { pathname } => init::cmd(&pathname),
        Commands::Log { commit } => Ok(log::cmd(&commit)),
    }
    .expect("Error!")
}
