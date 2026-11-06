# Contributing

Thanks for your interest in turtlebean. It's early, so please open an issue
to discuss a change before writing a large pull request.

## Clean-room rule

turtlebean is MIT licensed and must stay free of copyleft code. By opening a
pull request you confirm that your change:

- doesn't contain, port, translate or paraphrase code from Python beancount
  (GPL-2.0), rustledger (GPL-3.0) or any other copyleft implementation,
  including their grammars and test files
- is based on the beancount documentation, published specifications, or
  observing how beancount behaves when run

If you've read the source of one of those projects closely, say so in the pull
request, and avoid working on the parts you read.

## Scope

turtlebean supports a defined subset of beancount (see the README). Input
outside the subset should fail with a clear error. Widening the subset is
welcome, but discuss it in an issue first.

## Pull requests

- Use a [conventional commit](https://www.conventionalcommits.org/) title,
  such as `feat: parse pad directives`. Pull requests are squash-merged with
  that title.
- Commits merged into `main` must be signed.
- Run `mise run fix-all`, `mise run check-all` and `mise run test` before
  pushing. CI runs the same checks.
- Add tests for every change. For semantic rules, add a test that compares the
  result with Python beancount.
- Only commit test files you wrote or generated with `bean-example`.
- Use British English in code, comments and docs.
