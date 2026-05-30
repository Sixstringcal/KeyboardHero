// Quick CLI tool to query the shortcut database.
// Usage: kbhero-query <executable> <menu-segment> [<menu-segment>...]
// Example: kbhero-query firefox Edit Copy
//          kbhero-query code View "Command Palette"
//          kbhero-query nautilus Go "Parent Folder"

use kbhero_core::{
    db::ShortcutDatabase,
    types::{AppIdentity, Platform},
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.len() < 2 {
        eprintln!("Usage: kbhero-query <executable> <menu-segment>...");
        eprintln!("       kbhero-query firefox Edit Copy");
        eprintln!("       kbhero-query code View \"Command Palette\"");
        std::process::exit(1);
    }

    let db = ShortcutDatabase::build().expect("shortcut database must be valid");

    let app = AppIdentity {
        executable:   args[0].clone(),
        window_title: None,
        pid:          0,
    };
    let path: Vec<String> = args[1..].to_vec();
    let platform = Platform::current();

    match db.lookup_menu(&app, &path, platform) {
        Some(m) => {
            println!("  Shortcut : {}", m.shortcut_keys);
            println!("  Action   : {}", m.action_name);
            println!("  Path     : {}", m.menu_path);
            println!("  Source   : {:?}", m.source);
            if !m.description.is_empty() {
                println!("  Desc     : {}", m.description);
            }
        }
        None => {
            println!("No shortcut found for: {} → {}", app.executable, path.join(" › "));
        }
    }
}
