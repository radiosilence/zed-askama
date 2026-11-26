//! A template crate for the analyser's end-to-end tests: `Shop` renders cleanly, and `Broken`
//! names a field `Product` does not have, which only rust-analyzer can see.

use askama::Template;

pub struct Product {
    /// What the product is called.
    pub name: String,
    pub price_pence: u32,
}

impl Product {
    pub fn price(&self) -> String {
        format!("£{}.{:02}", self.price_pence / 100, self.price_pence % 100)
    }
}

#[derive(Template)]
#[template(path = "shop.html")]
pub struct Shop {
    pub title: String,
    pub products: Vec<Product>,
}

#[derive(Template)]
#[template(path = "broken.html")]
pub struct Broken {
    pub products: Vec<Product>,
}
