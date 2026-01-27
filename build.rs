fn main() {
    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .compile(&["proto/chess_bot.proto"], &["proto"])
        .unwrap_or_else(|e| panic!("Failed to compile protos: {:?}", e));
}
