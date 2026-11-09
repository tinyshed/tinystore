"""Search as docs/guides/search/README.md has it, through each way a store is reached.

Documents set by id and found by their words, a vector, or both, kept to what
a where passes, an index read again beside itself, a table's index, and an
index dropped.
"""

from __future__ import annotations

import struct
from array import array
from dataclasses import dataclass, field
from typing import Any

import pytest

import tinystore
from tinystore import Hybrid, Vectors, search


@dataclass
class Article:
    title: str
    body: str


@dataclass
class Product:
    title: str
    category: str | None = None
    price: int | None = None
    tags: list[str] = field(default_factory=list[str])


def ids(result: tinystore.Result[Any]) -> list[str]:
    return [hit.id for hit in result.hits]


async def test_documents_set_by_id_are_found_by_any_of_their_words_the_best_first(store: tinystore.Store) -> None:
    articles = store.index("articles", Article, fields={"title": 2, "body": 1})
    await articles.set("a", Article("Sourdough bread", "flour, water and salt"))
    await articles.set_many([("b", Article("Rye", "bread from rye flour")), ("c", Article("Butter", "milk and cream"))])
    result = await articles.search("bread salt")
    assert ids(result) == ["a", "b"]
    assert result.hits[0].value == Article("Sourdough bread", "flour, water and salt")
    assert len((await articles.search(text="flour", limit=1, offset=1)).hits) == 1
    assert (await articles.search("Sourdouhg")).corrected == "sourdough"
    assert await articles.get("c") == Article("Butter", "milk and cream")
    assert await articles.delete("c")
    assert await articles.get("c") is None
    assert not await articles.delete("c")


@dataclass
class Course:
    name: str


# contract: SRCH-11
async def test_a_word_typed_halfway_finds_the_words_it_begins_with_prefix_and_nothing_without(
    store: tinystore.Store,
) -> None:
    courses = store.index("courses", Course, fields={"name": 1})
    names = [
        "Cryptographic methods of information security",
        "Cryptanalysis",
        "Cryptography",
        "Foundations of cryptography",
    ]
    await courses.set_many([(str(n), Course(name)) for n, name in enumerate(names, start=1)])
    assert ids(await courses.search("crypt")) == []
    assert ids(await courses.search("crypt", prefix=True)) == ["2", "3", "4", "1"]
    assert ids(await courses.search("cryptograph", prefix=True)) == ["3", "4", "1"]
    assert ids(await courses.search("crypt ", prefix=True)) == []


# contract: SRCH-12
async def test_a_word_typed_with_the_other_layout_on_is_read_as_its_keys_type_there_and_corrected_says_so(
    store: tinystore.Store,
) -> None:
    courses = store.index("catalogue", Course, fields={"name": 1}, layouts=["ru", "en"])
    names = [
        "Криптографические методы защиты информации",
        "Линейная алгебра",
        "Python для анализа данных",
        "Инженерная графика",
    ]
    await courses.set_many([(str(n), Course(name)) for n, name in enumerate(names, start=1)])

    async def found(text: str) -> tuple[list[str], str | None]:
        result = await courses.search(text, prefix=True)
        return ids(result), result.corrected

    assert await found("rhbgnj") == (["1"], "крипто")
    assert await found("kby fku") == (["2"], "kby алг")
    assert await found("by; uhfa") == (["4"], "by; граф")
    assert await found("знерщт") == (["3"], "python")
    assert await found("python") == (["3"], None)


# contract: SRCH-13
async def test_prefix_every_reads_each_word_as_a_start_a_document_holding_one_for_each_first(
    store: tinystore.Store,
) -> None:
    courses = store.index("shortened", Course, fields={"name": 1})
    names = ["Линейная алгебра", "Алгоритмизация и программирование", "Проектирование технологических линий"]
    await courses.set_many([(str(n), Course(name)) for n, name in enumerate(names, start=1)])
    assert ids(await courses.search("лин алг", prefix="every")) == ["1", "3", "2"]
    assert ids(await courses.search("алг лин", prefix="every")) == ["1", "3", "2"]
    assert ids(await courses.search("лин алгебра", prefix="every")) == ["1", "3"]
    assert ids(await courses.search("линейная алг", prefix="every")) == ["1", "2"]
    assert ids(await courses.search("лин алг", prefix=True)) == ["3", "1", "2"]


@dataclass
class Listed:
    name: str
    short: str
    tags: list[str] = field(default_factory=list[str])


# contract: SRCH-14
async def test_a_hit_says_where_it_matched_in_code_points_that_slice_its_own_strings(
    store: tinystore.Store,
) -> None:
    catalog = store.index("catalog", Listed, fields={"name": 2, "short": 1, "tags": 1})
    await catalog.set_many(
        [
            ("1", Listed("Линейная алгебра и аналитическая геометрия", "ЛААГ")),
            ("2", Listed("Криптографические методы защиты информации", "КМЗИ")),
            ("3", Listed("🦀 Ёлка", "", ["ёлки", "пальмы"])),
        ]
    )

    def sliced(value: Listed, match: tinystore.Match) -> str:
        text = value.tags[match.item or 0] if match.field == "tags" else getattr(value, match.field)
        return text[match.start : match.end]

    async def shown(text: str) -> list[tuple[str, str, str, str]]:
        result = await catalog.search(text, prefix=True, matches=True)
        return [(hit.id, m.field, sliced(hit.value, m), m.word) for hit in result.hits for m in hit.matches or []]

    assert await shown("алгебре") == [("1", "name", "алгебра", "алгебре")]
    assert await shown("крипто") == [("2", "name", "Крипто", "крипто")]
    assert await shown("криптографичесике методы") == [
        ("2", "name", "Криптографические", "криптографичесике"),
        ("2", "name", "методы", "методы"),
    ]
    # the crab before Ёлка is one code point, where UTF-16 counts two
    tree = (await catalog.search("елка", matches=True)).hits[0]
    assert tree.matches == [
        tinystore.Match("name", 2, 6, "елка"),
        tinystore.Match("tags", 0, 4, "елка", item=0),
    ]
    assert (await catalog.search("елка")).hits[0].matches is None


async def test_a_hit_keeps_each_half_of_its_score_and_a_floor_keeps_a_far_vector_out_of_the_sum(
    store: tinystore.Store,
) -> None:
    opinions = store.index("opinions", Product, fields=["title"], vectors=Vectors(dimensions=3))
    await opinions.set("dota", Product("dota is dead"), vector=[1, 0, 0])
    await opinions.set("rain", Product("loves the rain"), vector=[0, 1, 0])
    await opinions.set("patch", Product("the new dota patch"), vector=[0, 0, 1])
    far = [-1, -1, -1]
    summed = await opinions.search("dota", vector=far)
    rain = next(hit for hit in summed.hits if hit.id == "rain")
    assert (rain.score, rain.text_score) == (0.5, None)
    assert rain.similarity == pytest.approx(-(3**-0.5))
    floored = await opinions.search("dota", vector=far, min_similarity=0.58)
    assert ids(floored) == ["dota", "patch"]
    assert all(hit.text_score is not None and hit.similarity is not None for hit in floored.hits)
    with pytest.raises(tinystore.InvalidError, match="rerank"):
        await opinions.search("dota", vector=far, min_similarity=0.58, hybrid=Hybrid(rerank=True))


async def test_a_vector_finds_the_nearest_by_its_cosine_and_with_words_both_are_summed_or_reranked(
    store: tinystore.Store,
) -> None:
    shoes = store.index(
        "shoes", Product, fields=["title"], filters=["category", "price", "tags"], vectors=Vectors(dimensions=3)
    )
    await shoes.set("a", Product("red shoes", "shoes", 50, ["sale"]), vector=[1, 0, 0])
    await shoes.set("b", Product("blue shoes", "boots", 150), vector=array("f", [0, 1, 0]))
    await shoes.set("c", Product("red hat"), vector=None)
    assert ids(await shoes.search(vector=[1, 0, 0])) == ["a", "b"]
    reranked = await shoes.search("red", vector=[0, 1, 0], hybrid=Hybrid(rerank=True))
    assert ids(reranked) == ["a", "c"]
    assert reranked.hits[1].score == -1
    cheap = await shoes.search("shoes", where={"category": ["shoes", "boots"], "price": {"lt": 100}})
    assert ids(cheap) == ["a"]
    assert ids(await shoes.search("red", where={"category": None})) == ["c"]
    either = await shoes.search("shoes", where=search.or_({"tags": "sale"}, {"price": {"gte": 100}}))
    assert ids(either) == ["a", "b"]
    with pytest.raises(tinystore.InvalidError):
        await shoes.set("d", Product("no vector said"))
    with pytest.raises(tinystore.InvalidError, match="not gt, gte, lt or lte"):
        await shoes.search("red", where={"price": {"above": 1}})


async def test_a_field_added_to_an_index_that_holds_documents_is_read_beside_it_until_it_switches_in(
    store: tinystore.Store,
) -> None:
    notes = store.index("notes", Article, fields=["title"])
    await notes.set("n", Article("bread", "salt"))
    both = store.index("notes", Article, fields=["title", "body"])
    await both.ready()
    assert await both.status() == tinystore.IndexStatus("ready")
    assert ids(await both.search("salt")) == ["n"]
    manual = store.index("notes", fields=["title"], rebuild="manual")
    assert (await manual.status()).state == "ready"
    refused = store.index("notes", fields=["title", "body", "tags"], rebuild="manual")
    with pytest.raises(tinystore.InvalidError):
        await refused.status()
    reindexed = await store.reindex("notes", fields=["title", "body"], rebuild="manual")
    await reindexed.ready()


@dataclass
class Row:
    id: int
    title: str
    done: int
    embedding: bytes | None


async def test_a_tables_index_finds_its_rows_each_hit_its_row_and_follows_every_write_to_the_table(
    store: tinystore.Store,
) -> None:
    db = await store.database(
        "shop",
        migrations={
            "0001_articles.sql": "create table articles (id integer primary key, title text not null, "
            "done integer not null default 0, embedding blob)"
        },
    )
    await db.exec("insert into articles (id, title) values (1, 'Sourdough bread')")
    articles = db.search_index(
        "articles_search",
        Row,
        table="articles",
        fields=["title"],
        filters=["done"],
        vectors=Vectors(column="embedding", dimensions=3),
    )
    await articles.ready()
    assert ids(await articles.search("bread")) == ["1"]
    embedding = struct.pack("<3f", 1, 0, 0)
    await db.exec("insert into articles (id, title, done, embedding) values (2, 'Rye bread', 1, ?)", embedding)
    done = await articles.search("bread", where={"done": True})
    assert ids(done) == ["2"]
    assert done.hits[0].value.title == "Rye bread"
    assert ids(await articles.search(vector=[1, 0, 0])) == ["2"]
    with pytest.raises(tinystore.InvalidError):
        await db.exec("insert into articles (id, title, embedding) values (3, 'ragged', ?)", bytes(5))
    assert await db.scalar(int, "select count(*) from articles where id = 3") == 0
    assert await db.drop_search_index("articles_search")
    with pytest.raises(tinystore.InvalidError):
        await articles.search("bread")


async def test_an_index_is_listed_and_dropped_with_every_document_it_holds(store: tinystore.Store) -> None:
    posts = store.index("posts", str)
    await posts.set("p", "hello")
    [post] = [index for index in await store.indexes() if index.name == "posts"]
    assert post.documents == 1
    assert post.opened is not None
    assert await store.drop_index("posts")
    assert not await store.drop_index("posts")
    with pytest.raises(tinystore.InvalidError):
        await posts.search("hello")


def test_a_where_is_written_as_the_wire_takes_it() -> None:
    assert search.where_json({"a": 1, "b": None, "c": ["x", "y"], "d": {"gte": 2}}) == (
        '[{"eq":["a",1]},{"missing":"b"},{"in":["c",["x","y"]]},{"range":["d",{"gte":2}]}]'
    )
    assert (
        search.where_json(search.or_({"a": 1}, {"b": True}))
        == '[{"or":[{"and":[{"eq":["a",1]}]},{"and":[{"eq":["b",true]}]}]}]'
    )
    with pytest.raises(tinystore.InvalidError):
        search.where_json({"a": float("nan")})
