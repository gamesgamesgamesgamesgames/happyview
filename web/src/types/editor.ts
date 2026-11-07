/** One file of the declarations an interpreter renders for editors. */
export interface DeclarationFile {
  path: string;
  contents: string;
}

/** `GET /admin/editor/declarations?language=<id>`. */
export interface EditorDeclarations {
  language_id: string;
  files: DeclarationFile[];
  /** What the input could not describe, such as a library whose surface does not parse. */
  notes: string[];
}
