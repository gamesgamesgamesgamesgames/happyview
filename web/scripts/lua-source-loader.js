// Turbopack has no built-in raw-text import, and the published loaders that
// provide one pull webpack in as a peer dependency for a one-line job.
module.exports = function luaSourceLoader(source) {
  return `export default ${JSON.stringify(source)};`;
};
