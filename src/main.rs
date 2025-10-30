use std::sync::{atomic::AtomicBool, Arc};

use signal_hook::{consts::SIGTERM, flag};

mod bot;
mod database;

fn main() {
    let term_flag = Arc::new(AtomicBool::new(false));
    flag::register_conditional_default(SIGTERM, Arc::clone(&term_flag)).unwrap();


}

fn safe_shutdown() {

}