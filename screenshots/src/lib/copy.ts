export interface SlideCopy {
  id: string;
  // `[[text]]` wraps text in the accent colour.
  headline: string;
  sub: string;
}

// The order here is the order on the product page. The first three show in search results.
export const SLIDES: SlideCopy[] = [
  {
    id: 'hero',
    headline: 'A [[floating]] Markdown note',
    sub: 'Press ⌥N in any app to write, and again to put it away.',
  },
  {
    id: 'format',
    headline: 'Formatting [[as you type]]',
    sub: 'Headings, checklists, bold text and code blocks take shape while you write.',
  },
  {
    id: 'actions',
    headline: 'Every action, [[one shortcut]]',
    sub: 'Press ⌘K to find any command without leaving the keyboard.',
  },
  {
    id: 'find',
    headline: 'Find [[any note]]',
    sub: 'Press ⌘P and search your notes by name or by what is in them.',
  },
  {
    id: 'tables',
    headline: 'Tables, [[cell by cell]]',
    sub: 'Insert a table and fill it in like a spreadsheet. It is saved as plain Markdown.',
  },
  {
    id: 'vim',
    headline: '[[Vim mode]] built in',
    sub: 'Edit with the keys you already know. Lists and tables move as whole pieces.',
  },
];
