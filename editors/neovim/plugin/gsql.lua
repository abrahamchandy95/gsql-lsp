if vim.g.loaded_gsql then
  return
end
vim.g.loaded_gsql = true

vim.api.nvim_create_user_command('GsqlBuildParser', function()
  require('gsql').build_parser()
end, { desc = 'Compile the tree-sitter-gsql parser into this plugin' })

local function install(args)
  require('gsql').install({ version = args.args ~= '' and args.args or nil })
end
vim.api.nvim_create_user_command('GsqlInstall', install, {
  nargs = '?',
  desc = 'Download the gsql-lsp release binary (optionally a tag such as v0.1.0) into stdpath("data")/gsql/bin',
})
vim.api.nvim_create_user_command('GsqlUpdate', install, {
  nargs = '?',
  desc = 'Same as :GsqlInstall: download the latest release again',
})
