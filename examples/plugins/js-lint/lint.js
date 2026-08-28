// A sample watoots plugin, in JavaScript.
//
//   npm install && npm run build
//
// Same world as examples/plugins/rust-lint, same host, same manifest. The
// point of having both is that the host does not know or care which language a
// plugin was written in -- the WIT world is the whole contract.

import { emit } from 'watoots:example/log@0.1.0';

export function name() {
  return 'js-lint';
}

export function lint(path, source) {
  // An import crossing, exactly like the Rust plugin's.
  emit('hint', `linting ${path}`);

  const diagnostics = [];

  source.split('\n').forEach((line, index) => {
    const lineNumber = index + 1;

    if (line.length > 80) {
      diagnostics.push({
        line: lineNumber,
        column: 81,
        severity: 'warning',
        message: `line is ${line.length} characters, over 80`,
      });
    }

    if (/[ \t]$/.test(line)) {
      diagnostics.push({
        line: lineNumber,
        column: line.length,
        severity: 'hint',
        message: 'trailing whitespace',
      });
    }

    const todo = line.indexOf('TODO');
    if (todo !== -1) {
      diagnostics.push({
        line: lineNumber,
        column: todo + 1,
        severity: 'error',
        message: 'unresolved TODO',
      });
    }
  });

  emit('hint', `${diagnostics.length} diagnostic(s)`);
  return diagnostics;
}
