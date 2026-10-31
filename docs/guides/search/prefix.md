# As you type

A search box that answers while its reader types sends a word typed halfway:
`crypt` on its way to `cryptography`. With `prefix`, Search reads the last
word of the text as the start of a word too, so the box finds every document
with a word that starts with it, as the reader types each letter.

```ts
const courses = store.index<{ name: string }>('courses', { fields: { name: 1 } })
await courses.setMany([
	{ id: 1, value: { name: 'Cryptographic methods of information security' } },
	{ id: 2, value: { name: 'Cryptanalysis' } },
	{ id: 3, value: { name: 'Cryptography' } },
	{ id: 4, value: { name: 'Foundations of cryptography' } },
])
const { hits } = await courses.search({ text: 'crypt', prefix: true })
hits.map(hit => hit.id) // ['2', '3', '4', '1']
```

```python
courses = store.index("courses", Course, fields={"name": 1})
await courses.set_many([("1", Course("Cryptographic methods of information security")), ("2", Course("Cryptanalysis"))])
result = await courses.search("crypt", prefix=True)
```

```go
courses, err := search.OpenIndex[Course](store, "courses", search.Fields{"name": 1})
result, err := courses.Search(ctx, "crypt", search.Prefix())
```

```rust
let courses = store.index::<Course>("courses").field("name", 1.0).open()?;
let found = courses.search("crypt").prefix().run()?;
```

Without `prefix`, `crypt` is a word of its own: it finds a course named
`Crypt`, and none of the four above.

## Which word is a prefix

Only the last word, and only while the reader may still be typing it:

| Text               | Read as                                                    |
|--------------------|------------------------------------------------------------|
| `crypt`            | `crypt`, and every word that starts with it                |
| `methods crypt`    | `methods` whole, then `crypt` and the words it begins      |
| `crypt ` (a space) | `crypt` whole: the reader finished the word                |
| `crypt,`           | `crypt` whole: a mark ends a word as a space does          |
| `c`                | `c` whole: one letter would begin half the words of a page |

A word is read as Search reads every word: in lower case, accents off, `ё` as
`е`. `Crypt` and `crypt` are the same prefix, in any script.

## How a prefix ranks

The words a prefix begins count as one word in each field: a document holding
two of them counts both, as it would one word written twice. They weigh half
their field's weight, less than the stem of a Cyrillic word, which weighs
three quarters, so a document holding the word typed comes before one holding
only a longer word, unless that word is far rarer or its field far shorter.

Among the words a prefix begins, the shortest field ranks first, as BM25 ranks
any word: `cryptograph` finds `Cryptography`, then `Foundations of
cryptography`, then `Cryptographic methods of information security`.

## Typos and a prefix

A word typed halfway is no typo while it begins a word the index holds:
`cardi` is one letter from `cards`, and still finds `cardigan` rather than
being read as `cards`. A last word that begins nothing the index holds is
corrected as any word is, and [`corrected`](typos.md) says so:
`cryptograhpy` reads as `cryptography`.

A typo inside a prefix is not forgiven as a prefix: a last word that begins
nothing is read only as the whole word nearest it, under the [rules of
typos](typos.md#the-rules), and its prefix is then that word.

## Limits and defaults

| What                         | Value                          |
|------------------------------|--------------------------------|
| `prefix`                     | off unless the search says     |
| the shortest prefix          | two letters                    |
| words a prefix is read as    | the 64 most held in each field |
| what a word it begins weighs | half its field's weight        |
| words before the last        | whole, always                  |

A prefix reads the postings of every word it begins before the search scores,
so a short prefix on an index of many documents reads more than a whole word
would. The 64 most held words bound it.
