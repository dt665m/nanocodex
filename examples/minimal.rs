use nanocodex::{Nanocodex, OpenAi};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    // One reusable runtime owns the shared client; every spawn is isolated.
    let nano = Nanocodex::builder()
        .openai(OpenAi::new(api_key)?)
        .instructions("Inspect the repository carefully and report only verified facts.")
        .build()?;
    let (agent, _events) = nano.spawn().build().await?;

    let turn = agent
        .prompt("Inspect this repository and summarize it.")
        .await?;
    let result = turn.result().await?;
    println!("{}", result.final_message());
    nano.shutdown().await?;
    Ok(())
}
