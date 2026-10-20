fn main() {
    let args: Vec<String> = std::env::args().collect();
    let db = storyforge_core::Db::open(&args[1]).expect("open database");
    let out = storyforge_core::export::export_all(&db).expect("export");
    println!("exported {} book(s)", out.len());
}
