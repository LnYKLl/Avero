mod paths;
// Not called yet: the iced UI (Task 8) will use it.
#[allow(dead_code)]
mod worker;

fn main() {
    let store = avero_core::store::Store::open(&paths::database_path()).expect("open database");
    let accounts = store.accounts().expect("list accounts");
    println!("{} compte(s)", accounts.len());
}
