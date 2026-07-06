declare namespace React.JSX {
  interface IntrinsicElements {
    'sequoia-comments': React.DetailedHTMLProps<
      React.HTMLAttributes<HTMLElement> & {
        'document-uri'?: string;
        'post-uri'?: string;
        depth?: string | number;
        hide?: string;
      },
      HTMLElement
    >;
  }
}
