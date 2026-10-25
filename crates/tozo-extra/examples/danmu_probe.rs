use tozo_extra::engines::bilibili;

#[tokio::main]
async fn main() {
    let bv = std::env::args().nth(1).unwrap_or_else(|| "BV1Gy4y1q7ZS".into());
    match bilibili::fetch_cid(&bv).await {
        Ok((aid, cid)) => {
            println!("cid ok: aid={aid} cid={cid}");
            match bilibili::fetch_segment(aid, cid, 1).await {
                Ok(list) => println!(
                    "segment ok: {} items, first={:?}",
                    list.len(),
                    list.first().map(|d| d.content.clone())
                ),
                Err(e) => println!("segment err: {e}"),
            }
        }
        Err(e) => println!("cid err: {e}"),
    }
}
