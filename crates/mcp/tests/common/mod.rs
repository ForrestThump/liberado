#[macro_export]
macro_rules! mcp_handler_stubs {
    () => {
        fn list_resources(&self) -> Vec<Resource> {
            Vec::new()
        }
        fn list_prompts(&self) -> Vec<Prompt> {
            Vec::new()
        }
        fn read_resource<'a>(
            &'a self,
            uri: &'a str,
            _ctx: &'a RequestContext,
        ) -> impl Future<Output = McpResult<ResourceResult>> + MaybeSend + 'a {
            let uri = uri.to_string();
            async move { Err(McpError::resource_not_found(&uri)) }
        }
        fn get_prompt<'a>(
            &'a self,
            name: &'a str,
            _args: Option<Value>,
            _ctx: &'a RequestContext,
        ) -> impl Future<Output = McpResult<PromptResult>> + MaybeSend + 'a {
            let name = name.to_string();
            async move { Err(McpError::prompt_not_found(&name)) }
        }
    };
}
