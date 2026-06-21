use crate::lsp;

pub(crate) fn lsp() -> Result<(), String> {
    let mut server = lsp::LspServer::new();
    server.run()
}
