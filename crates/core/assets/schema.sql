-- Story Forge — схема базы
-- Принцип: проза глав живёт в файлах, все структурные факты — здесь.
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- ─────────────────────────── КНИГА ───────────────────────────

CREATE TABLE IF NOT EXISTS book (
  id            INTEGER PRIMARY KEY,
  slug          TEXT UNIQUE NOT NULL,      -- имя папки
  title         TEXT NOT NULL,
  root_path     TEXT NOT NULL,             -- абсолютный путь к папке книги
  genre         TEXT,
  pov           TEXT,
  tone          TEXT,
  language      TEXT DEFAULT 'ru',
  active_style  TEXT,                      -- styles/<имя>.md
  min_chars     INTEGER DEFAULT 10000,     -- порог площадки, знаков без пробелов
  target_chars  INTEGER DEFAULT 12000,
  premise       TEXT,
  created_at    TEXT DEFAULT (datetime('now')),
  updated_at    TEXT DEFAULT (datetime('now'))
);

-- Свободные markdown-документы, которые не раскладываются в структуру
-- (book.md, direction.md, arc2_vino.md, factions.md, профили стиля...).
-- Гарантия, что при импорте ничего не потеряется.
CREATE TABLE IF NOT EXISTS doc (
  id         INTEGER PRIMARY KEY,
  book_id    INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  kind       TEXT NOT NULL,                -- book | direction | arc_plan | style | factions | other
  rel_path   TEXT NOT NULL,
  title      TEXT,
  body       TEXT NOT NULL,
  updated_at TEXT DEFAULT (datetime('now')),
  UNIQUE(book_id, rel_path)
);

-- ═══════════════════ ЛИНИЯ ВРЕМЕНИ ═══════════════════
--
-- Единая ось, к которой привязано всё: главы, события, факты.
-- Номер главы для этого не годится — порядок глав не равен порядку событий.
-- В «Эхе чужих миров» шестая глава идёт параллельно четвёртой и пятой,
-- а первые три вообще в другом мире. Привязка к моменту это учитывает.

CREATE TABLE IF NOT EXISTS moment (
  id         INTEGER PRIMARY KEY,
  book_id    INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  seq        REAL NOT NULL,             -- порядок на оси; дробный, чтобы вставлять между
  label      TEXT NOT NULL,             -- «Гринтаун, день 3, вечер»
  world      TEXT,                      -- реальный мир | мир книги — разные потоки времени
  story_date TEXT,
  note       TEXT
);

CREATE INDEX IF NOT EXISTS idx_moment_book ON moment(book_id, seq);

-- ─────────────────────────── АРКИ ───────────────────────────

CREATE TABLE IF NOT EXISTS arc (
  id       INTEGER PRIMARY KEY,
  book_id  INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  ext_id   TEXT,                           -- id из data.json (a_vino)
  name     TEXT NOT NULL,
  color    TEXT,
  ord      INTEGER DEFAULT 0,
  UNIQUE(book_id, ext_id)
);

-- ─────────────────────────── ПЕРСОНАЖИ ───────────────────────────

CREATE TABLE IF NOT EXISTS character (
  id             INTEGER PRIMARY KEY,
  book_id        INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  ext_id         TEXT,                     -- id из data.json (c_miya)
  name           TEXT NOT NULL,
  aliases        TEXT,
  is_main        INTEGER DEFAULT 0,
  status         TEXT,                     -- жив / мёртв / ...
  age            TEXT,
  gender         TEXT,
  group_name     TEXT,
  summary        TEXT,                     -- «Характер: текущий»
  character_history TEXT,                  -- «История изменений»
  voice          TEXT,                     -- манера речи
  voice_rules    TEXT,                     -- чего никогда не скажет / ограничения
  atmosphere_desc  TEXT,
  atmosphere_level INTEGER DEFAULT 0,      -- 0..5
  appearance     TEXT,
  goals          TEXT,
  beliefs        TEXT,
  knows          TEXT,                     -- раздел «Знает / Не знает»
  notes          TEXT,                     -- «Прочее», прозвища и т.п.
  arc_from       TEXT,                     -- арка: из
  arc_to         TEXT,                     -- арка: в
  arc_stage      TEXT,                     -- стадия сейчас
  source_path    TEXT,
  UNIQUE(book_id, name)
);

CREATE INDEX IF NOT EXISTS idx_character_book ON character(book_id);

-- Эволюция персонажа по главам: одна строка = один факт, привязанный к главе.
CREATE TABLE IF NOT EXISTS character_state (
  id            INTEGER PRIMARY KEY,
  character_id  INTEGER NOT NULL REFERENCES character(id) ON DELETE CASCADE,
  chapter_no    INTEGER,                   -- NULL = до начала книги / бэкстори
  kind          TEXT DEFAULT 'event',      -- event | trait | appearance | knowledge
  text          TEXT NOT NULL,
  hidden        INTEGER DEFAULT 0,         -- 🔒 закадровое, читателю не показано
  ord           INTEGER DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_charstate_char ON character_state(character_id, chapter_no);

-- ─────────────────────── ОТНОШЕНИЯ ───────────────────────

CREATE TABLE IF NOT EXISTS relationship (
  id           INTEGER PRIMARY KEY,
  book_id      INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  from_char    INTEGER NOT NULL REFERENCES character(id) ON DELETE CASCADE,
  to_char      INTEGER REFERENCES character(id) ON DELETE CASCADE,
  to_label     TEXT,                       -- если адресат не персонаж (группа, семья)
  kind         TEXT,                       -- дружба / вражда / романтика / группа
  current_state TEXT NOT NULL,
  history      TEXT,
  is_mutual    INTEGER DEFAULT 1,
  UNIQUE(book_id, from_char, to_char, to_label)
);

CREATE INDEX IF NOT EXISTS idx_rel_book ON relationship(book_id);

CREATE TABLE IF NOT EXISTS relationship_state (
  id              INTEGER PRIMARY KEY,
  relationship_id INTEGER NOT NULL REFERENCES relationship(id) ON DELETE CASCADE,
  chapter_no      INTEGER,
  to_chapter      INTEGER,
  from_moment     INTEGER REFERENCES moment(id) ON DELETE SET NULL,
  to_moment       INTEGER REFERENCES moment(id) ON DELETE SET NULL,
  state           TEXT NOT NULL,
  note            TEXT,
  event_id        INTEGER REFERENCES event(id) ON DELETE SET NULL
);

-- ─────────────────────────── ГЛАВЫ ───────────────────────────

CREATE TABLE IF NOT EXISTS chapter (
  id             INTEGER PRIMARY KEY,
  book_id        INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  ext_id         TEXT,                     -- ch7
  number         INTEGER NOT NULL,
  title          TEXT,
  file_path      TEXT,                     -- относительный путь к прозе
  words          INTEGER DEFAULT 0,
  chars_no_space INTEGER DEFAULT 0,
  status         TEXT DEFAULT 'план',      -- план | черновик | написано
  arc_id         INTEGER REFERENCES arc(id) ON DELETE SET NULL,
  pov            TEXT,
  summary        TEXT,                     -- из summaries/chNNN.md
  plan           TEXT,                     -- замысел до написания
  from_moment    INTEGER REFERENCES moment(id) ON DELETE SET NULL,
  to_moment      INTEGER REFERENCES moment(id) ON DELETE SET NULL,
  updated_at     TEXT DEFAULT (datetime('now')),
  UNIQUE(book_id, number)
);

-- ─────────────────────────── СОБЫТИЯ ───────────────────────────

CREATE TABLE IF NOT EXISTS event (
  id           INTEGER PRIMARY KEY,
  book_id      INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  ext_id       TEXT,
  chapter_id   INTEGER REFERENCES chapter(id) ON DELETE SET NULL,
  chapter_end_id INTEGER REFERENCES chapter(id) ON DELETE SET NULL,
  title        TEXT NOT NULL,
  summary      TEXT,
  status       TEXT DEFAULT 'план',        -- план | написано
  hidden       INTEGER DEFAULT 0,
  weight       INTEGER DEFAULT 0,          -- объём в словах, для размера бабла
  arc_id       INTEGER REFERENCES arc(id) ON DELETE SET NULL,
  story_date   TEXT,
  moment_id    INTEGER REFERENCES moment(id) ON DELETE SET NULL,
  x            REAL,                        -- позиция на холсте
  y            REAL,
  UNIQUE(book_id, ext_id)
);

CREATE INDEX IF NOT EXISTS idx_event_book ON event(book_id);
CREATE INDEX IF NOT EXISTS idx_event_chapter ON event(chapter_id);

CREATE TABLE IF NOT EXISTS event_character (
  event_id     INTEGER NOT NULL REFERENCES event(id) ON DELETE CASCADE,
  character_id INTEGER NOT NULL REFERENCES character(id) ON DELETE CASCADE,
  state_snapshot TEXT,                     -- cstate: каким персонаж был в этот момент
  PRIMARY KEY (event_id, character_id)
);

CREATE TABLE IF NOT EXISTS event_link (
  from_event INTEGER NOT NULL REFERENCES event(id) ON DELETE CASCADE,
  to_event   INTEGER NOT NULL REFERENCES event(id) ON DELETE CASCADE,
  kind       TEXT DEFAULT 'causal',
  PRIMARY KEY (from_event, to_event)
);

-- ─────────────────── СКРЫТЫЕ ЛИНИИ И НАМЁКИ ───────────────────

CREATE TABLE IF NOT EXISTS hidden_line (
  id         INTEGER PRIMARY KEY,
  book_id    INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  code       TEXT,                          -- H1
  name       TEXT NOT NULL,
  truth      TEXT,                          -- ПРАВДА
  who_knows  TEXT,
  reader_learns TEXT,
  status     TEXT,
  UNIQUE(book_id, code)
);

-- закадровая хронология скрытой линии
CREATE TABLE IF NOT EXISTS hidden_beat (
  id             INTEGER PRIMARY KEY,
  hidden_line_id INTEGER NOT NULL REFERENCES hidden_line(id) ON DELETE CASCADE,
  chapter_no     INTEGER,
  offscreen      TEXT NOT NULL,             -- событие за кадром
  visible_effect TEXT,                      -- видимое следствие
  planned        INTEGER DEFAULT 0          -- 1 = ещё не реализовано
);

-- посеянные намёки
CREATE TABLE IF NOT EXISTS hint (
  id             INTEGER PRIMARY KEY,
  hidden_line_id INTEGER REFERENCES hidden_line(id) ON DELETE CASCADE,
  chapter_no     INTEGER,
  text           TEXT NOT NULL,
  carrier        TEXT,                      -- через что / через кого
  level          INTEGER DEFAULT 0          -- 0..5
);

-- ─────────────────── ТАЙНЫ И ЗНАНИЯ ───────────────────

CREATE TABLE IF NOT EXISTS secret (
  id           INTEGER PRIMARY KEY,
  book_id      INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  ext_no       TEXT,
  title        TEXT NOT NULL,
  truth        TEXT,
  reader_knows TEXT,
  reveal_plan  TEXT
);

CREATE TABLE IF NOT EXISTS secret_knowledge (
  secret_id    INTEGER NOT NULL REFERENCES secret(id) ON DELETE CASCADE,
  character_id INTEGER REFERENCES character(id) ON DELETE CASCADE,
  label        TEXT,                        -- если не персонаж («весь клуб»)
  stance       TEXT NOT NULL                -- знает | заблуждается | не знает
);

-- ─────────────────── ПРОМИСЫ (ружья) ───────────────────

CREATE TABLE IF NOT EXISTS promise (
  id             INTEGER PRIMARY KEY,
  book_id        INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  ext_no         TEXT,
  title          TEXT NOT NULL,
  planted_ch     INTEGER,
  expected_payoff TEXT,
  status         TEXT DEFAULT 'ожидает',
  fired_ch       INTEGER
);

-- ─────────────────── ЛЕНТА АКТИВНОСТИ ИИ (MCP) ───────────────────
-- Внешняя модель может работать через MCP (Claude Code, Cowork), а сюда,
-- в /mcp, только присылает вызовы. Эта таблица — лог того, что ИИ реально
-- сделал с книгой через MCP: не переписка, а факт действия. Пишут сюда
-- write-инструменты в server/lib/mcpCore.js (поле `activity` у каждого).

CREATE TABLE IF NOT EXISTS mcp_activity (
  id         INTEGER PRIMARY KEY,
  book_id    INTEGER REFERENCES book(id) ON DELETE CASCADE,
  tool       TEXT NOT NULL,
  summary    TEXT NOT NULL,
  detail     TEXT,                           -- JSON: аргументы вызова, для отладки
  created_at TEXT DEFAULT (datetime('now'))
);

-- ─────────────────── МИР ───────────────────

CREATE TABLE IF NOT EXISTS world_rule (
  id       INTEGER PRIMARY KEY,
  book_id  INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  category TEXT,
  text     TEXT NOT NULL,
  locked   INTEGER DEFAULT 0,              -- 🔒 нерушимое
  ord      INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS location (
  id          INTEGER PRIMARY KEY,
  book_id     INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  name        TEXT NOT NULL,
  kind        TEXT,
  where_desc  TEXT,
  description TEXT,
  who_here    TEXT,
  events_here TEXT,
  source_path TEXT,
  UNIQUE(book_id, name)
);

CREATE TABLE IF NOT EXISTS glossary (
  id       INTEGER PRIMARY KEY,
  book_id  INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  term     TEXT NOT NULL,
  meaning  TEXT,
  section  TEXT,                            -- имена / персонажи книги / термины
  UNIQUE(book_id, term, section)
);

CREATE TABLE IF NOT EXISTS timeline_entry (
  id          INTEGER PRIMARY KEY,
  book_id     INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  chapter_no  INTEGER,
  story_date  TEXT,
  moment_id   INTEGER REFERENCES moment(id) ON DELETE SET NULL,
  description TEXT NOT NULL,
  who         TEXT,
  hidden      INTEGER DEFAULT 0,
  ord         INTEGER DEFAULT 0
);

-- ─────────────────── СРЕЗ «СЕЙЧАС» ───────────────────

CREATE TABLE IF NOT EXISTS current_state (
  id         INTEGER PRIMARY KEY,
  book_id    INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  last_chapter INTEGER,
  story_date TEXT,
  positions  TEXT,                          -- markdown-таблица «где кто»
  conflicts  TEXT,
  open_questions TEXT,
  updated_at TEXT DEFAULT (datetime('now'))
);

-- ═══════════════════ ФАКТЫ ВО ВРЕМЕНИ ═══════════════════
--
-- Хранится не «текущее значение», а факт с интервалом действия:
-- верен с одного момента до другого. Тогда состояние на любую точку
-- истории — это один запрос, а не перечитывание книги.

-- Словарь видов факта — дерево категорий.
--
-- Отдельной таблицей, а не свободной строкой: иначе «предмет», «предметы»
-- и «Предмет» тихо разъедутся на три кучки, а по этому полю идёт фильтрация.
-- Иерархия нужна, чтобы спрашивать «весь облик» и получать одежду,
-- причёску и приметы разом, не перечисляя их поимённо.
--
-- slotted  — 1: у субъекта одно текущее значение на слот (новое закрывает старое)
--            0: копится навсегда (события — это биография, её не вытесняют)
-- key_hint — какие слоты обычно бывают у этого вида; подсказка интерфейсу и модели
CREATE TABLE IF NOT EXISTS fact_kind (
  code     TEXT PRIMARY KEY,
  title    TEXT NOT NULL,
  parent   TEXT REFERENCES fact_kind(code),
  slotted  INTEGER NOT NULL DEFAULT 1,
  key_hint TEXT,
  ord      INTEGER DEFAULT 0
);

-- ── Корневые ветви
INSERT OR IGNORE INTO fact_kind (code, title, parent, slotted, key_hint, ord) VALUES
  ('внешность',     'Облик',        NULL, 1, NULL, 10),
  ('предмет',       'При себе',     NULL, 1, NULL, 20),
  ('тело',          'Тело',         NULL, 1, NULL, 30),
  ('нутро',         'Внутреннее',   NULL, 1, NULL, 40),
  ('умения',        'Умения',       NULL, 1, NULL, 50),
  ('социальное',    'Социальное',   NULL, 1, NULL, 60),
  ('положение',     'Положение',    NULL, 1, NULL, 70),
  ('сюжет',         'Сюжет',        NULL, 1, NULL, 80);

-- ── Облик
INSERT OR IGNORE INTO fact_kind (code, title, parent, slotted, key_hint, ord) VALUES
  ('телосложение',  'Телосложение', 'внешность', 1, 'рост · комплекция', 11),
  ('лицо',          'Лицо',         'внешность', 1, 'глаза · черты · кожа', 12),
  ('причёска',      'Причёска',     'внешность', 1, NULL, 13),
  ('одежда',        'Одежда',       'внешность', 1, 'верх · низ · обувь · верхняя · головной убор', 14),
  ('аксессуар',     'Аксессуары',   'внешность', 1, 'украшение · очки · часы · сумка', 15),
  ('примета',       'Особые приметы','внешность',1, 'шрам · татуировка · родинка · протез', 16);

-- ── При себе
INSERT OR IGNORE INTO fact_kind (code, title, parent, slotted, key_hint, ord) VALUES
  ('оружие',        'Оружие',       'предмет', 1, 'нож · пистолет · меч', 21),
  ('документ',      'Документы',    'предмет', 1, 'паспорт · письмо · карта', 22),
  ('ценность',      'Деньги и ценности','предмет',1,'кошелёк · украшение', 23),
  ('артефакт',      'Особые предметы','предмет',1, NULL, 24),
  ('расходник',     'Расходники',   'предмет', 1, 'еда · зелье · патроны', 25),
  ('транспорт',     'Транспорт',    'предмет', 1, NULL, 26);

-- ── Тело
INSERT OR IGNORE INTO fact_kind (code, title, parent, slotted, key_hint, ord) VALUES
  ('рана',          'Раны',         'тело', 1, 'где именно', 31),
  ('болезнь',       'Болезни',      'тело', 1, NULL, 32),
  ('самочувствие',  'Самочувствие', 'тело', 1, 'усталость · голод · опьянение', 33),
  ('возраст',       'Возраст',      'тело', 1, NULL, 34);

-- ── Внутреннее
INSERT OR IGNORE INTO fact_kind (code, title, parent, slotted, key_hint, ord) VALUES
  ('черта',         'Черта характера','нутро', 1, NULL, 41),
  ('эмоция',        'Состояние',    'нутро', 1, NULL, 42),
  ('травма',        'Травма',       'нутро', 1, NULL, 43),
  ('страх',         'Страхи',       'нутро', 1, NULL, 44),
  ('убеждение',     'Убеждения',    'нутро', 1, NULL, 45),
  ('привычка',      'Привычки и тики','нутро',1, NULL, 46),
  ('цель',          'Цели',         'нутро', 1, 'ближняя · дальняя', 47);

-- ── Умения
INSERT OR IGNORE INTO fact_kind (code, title, parent, slotted, key_hint, ord) VALUES
  ('навык',         'Навыки',       'умения', 1, 'боевой · ремесло · общение', 51),
  ('эрудиция',      'Знания и образование','умения',1,'лор · наука · язык', 52),
  ('способность',   'Способности',  'умения', 1, NULL, 53),
  ('ограничение',   'Цена и ограничения','умения',1,'к какой способности', 54),
  ('уровень',       'Уровень или ранг','умения',1, NULL, 55);

-- ── Социальное
INSERT OR IGNORE INTO fact_kind (code, title, parent, slotted, key_hint, ord) VALUES
  ('имя',           'Имя и обращения','социальное',1,'настоящее · псевдоним · прозвище', 61),
  ('статус',        'Статус',       'социальное', 1, 'титул · должность', 62),
  ('принадлежность','Принадлежность','социальное',1,'группа · фракция', 63),
  ('репутация',     'Репутация',    'социальное', 1, 'у кого именно', 64),
  ('долг',          'Долги и клятвы','социальное',1, 'перед кем', 65),
  ('состав',        'Состав группы','социальное', 1, NULL, 66);

-- ── Положение
INSERT OR IGNORE INTO fact_kind (code, title, parent, slotted, key_hint, ord) VALUES
  ('место',         'Где находится','положение', 1, NULL, 71),
  ('дом',           'Дом или база', 'положение', 1, NULL, 72),
  ('маршрут',       'Куда направляется','положение',1, NULL, 73);

-- ── Сюжет
INSERT OR IGNORE INTO fact_kind (code, title, parent, slotted, key_hint, ord) VALUES
  ('обязательство', 'Незакрытое',   'сюжет', 1, NULL, 81),
  ('правило',       'Правило мира', 'сюжет', 1, NULL, 82),
  ('событие',       'События',      'сюжет', 0, NULL, 83);

-- Виды, заведённые до появления дерева, подцепляем к ветвям.
UPDATE fact_kind SET parent='нутро',      ord=41 WHERE code='черта'         AND parent IS NULL;
UPDATE fact_kind SET parent='нутро',      ord=42 WHERE code='эмоция'        AND parent IS NULL;
UPDATE fact_kind SET parent='нутро',      ord=47 WHERE code='цель'          AND parent IS NULL;
UPDATE fact_kind SET parent='социальное', ord=61 WHERE code='имя'           AND parent IS NULL;
UPDATE fact_kind SET parent='социальное', ord=62 WHERE code='статус'        AND parent IS NULL;
UPDATE fact_kind SET parent='социальное', ord=66 WHERE code='состав'        AND parent IS NULL;
UPDATE fact_kind SET parent='положение',  ord=71 WHERE code='место'         AND parent IS NULL;
UPDATE fact_kind SET parent='сюжет',      ord=81 WHERE code='обязательство' AND parent IS NULL;
UPDATE fact_kind SET parent='сюжет',      ord=82 WHERE code='правило'       AND parent IS NULL;
UPDATE fact_kind SET parent='сюжет',      ord=83, slotted=0 WHERE code='событие' AND parent IS NULL;

CREATE TABLE IF NOT EXISTS fact (
  id           INTEGER PRIMARY KEY,
  book_id      INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,

  subject_type TEXT NOT NULL,           -- character | location | group | world | plot
  subject_id   INTEGER,
  subject_name TEXT,                    -- если субъект не заведён отдельной записью

  kind         TEXT NOT NULL REFERENCES fact_kind(code),
  key          TEXT,                    -- имя слота: «оружие», «одежда», «дом»
  value        TEXT NOT NULL,
  weight       INTEGER DEFAULT 1,       -- 0 фон · 1 обычный · 2 ключевой для персонажа

  from_moment  INTEGER REFERENCES moment(id) ON DELETE SET NULL,
  to_moment    INTEGER REFERENCES moment(id) ON DELETE SET NULL,
  from_chapter INTEGER,                 -- дублируется для удобства чтения и фильтров
  to_chapter   INTEGER,
  -- Порядок на оси, скопированный из moment. Денормализация ради скорости:
  -- на десятках тысяч фактов два JOIN к moment в каждом запросе среза
  -- обходятся дороже, чем поддержка двух колонок триггерами.
  from_seq     REAL,
  to_seq       REAL,

  event_id     INTEGER REFERENCES event(id) ON DELETE SET NULL,
  status       TEXT DEFAULT 'канон',    -- канон | план (для ненаписанных глав)
  hidden       INTEGER DEFAULT 0,       -- 🔒 читателю не показано
  source       TEXT DEFAULT 'вручную',  -- импорт | вручную | ИИ
  note         TEXT,
  source_chapter_id INTEGER REFERENCES chapter(id) ON DELETE SET NULL,
  source_quote TEXT,
  created_at   TEXT DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_fact_subject ON fact(book_id, subject_type, subject_id);
CREATE INDEX IF NOT EXISTS idx_fact_slice   ON fact(book_id, from_seq, to_seq);
CREATE INDEX IF NOT EXISTS idx_fact_slot    ON fact(book_id, subject_type, subject_id, kind, key, to_seq);
CREATE INDEX IF NOT EXISTS idx_fact_kind    ON fact(book_id, kind, from_seq);

-- Поддержка from_seq/to_seq: значение берётся из момента при вставке и правке,
-- а если сам момент сдвинули по оси — обновляются все зависимые записи.
CREATE TRIGGER IF NOT EXISTS fact_seq_ins AFTER INSERT ON fact
BEGIN
  UPDATE fact SET
    from_seq = (SELECT seq FROM moment WHERE id = NEW.from_moment),
    to_seq   = (SELECT seq FROM moment WHERE id = NEW.to_moment)
  WHERE id = NEW.id;
END;

CREATE TRIGGER IF NOT EXISTS fact_seq_upd AFTER UPDATE OF from_moment, to_moment ON fact
BEGIN
  UPDATE fact SET
    from_seq = (SELECT seq FROM moment WHERE id = NEW.from_moment),
    to_seq   = (SELECT seq FROM moment WHERE id = NEW.to_moment)
  WHERE id = NEW.id;
END;


-- Знание вынесено отдельно: у него трёхзначная позиция, которую
-- не выразить одним значением. Заблуждение — не то же, что незнание:
-- персонаж действует исходя из ложного, и это двигает сюжет.
CREATE TABLE IF NOT EXISTS knowledge_stance (
  code  TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  ord   INTEGER DEFAULT 0
);

INSERT OR IGNORE INTO knowledge_stance (code, title, ord) VALUES
  ('знает',        'Знает правду',              1),
  ('подозревает',  'Подозревает',               2),
  ('заблуждается', 'Верит в неверное',          3),
  ('не знает',     'Не знает',                  4);

CREATE TABLE IF NOT EXISTS knowledge (
  id           INTEGER PRIMARY KEY,
  book_id      INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  character_id INTEGER REFERENCES character(id) ON DELETE CASCADE,
  holder_label TEXT,                    -- «город», «клуб» — только для настоящих групп

  secret_id    INTEGER REFERENCES secret(id) ON DELETE CASCADE,
  topic        TEXT,                    -- свободная тема, если это не тайна из списка

  stance       TEXT NOT NULL REFERENCES knowledge_stance(code),
  belief       TEXT,                    -- во что именно верит, если заблуждается

  from_moment  INTEGER REFERENCES moment(id) ON DELETE SET NULL,
  to_moment    INTEGER REFERENCES moment(id) ON DELETE SET NULL,
  from_chapter INTEGER,
  to_chapter   INTEGER,
  from_seq     REAL,
  to_seq       REAL,

  event_id     INTEGER REFERENCES event(id) ON DELETE SET NULL,
  source       TEXT DEFAULT 'вручную',
  note         TEXT,
  source_chapter_id INTEGER REFERENCES chapter(id) ON DELETE SET NULL,
  source_quote TEXT
);

CREATE INDEX IF NOT EXISTS idx_knowledge_char  ON knowledge(book_id, character_id, to_seq);
CREATE INDEX IF NOT EXISTS idx_knowledge_slice ON knowledge(book_id, from_seq, to_seq);

CREATE TRIGGER IF NOT EXISTS knowledge_seq_ins AFTER INSERT ON knowledge
BEGIN
  UPDATE knowledge SET
    from_seq = (SELECT seq FROM moment WHERE id = NEW.from_moment),
    to_seq   = (SELECT seq FROM moment WHERE id = NEW.to_moment)
  WHERE id = NEW.id;
END;

CREATE TRIGGER IF NOT EXISTS knowledge_seq_upd AFTER UPDATE OF from_moment, to_moment ON knowledge
BEGIN
  UPDATE knowledge SET
    from_seq = (SELECT seq FROM moment WHERE id = NEW.from_moment),
    to_seq   = (SELECT seq FROM moment WHERE id = NEW.to_moment)
  WHERE id = NEW.id;
END;

-- ─────────────────── ГРАФ-БЛЮПРИНТ ───────────────────

CREATE TABLE IF NOT EXISTS node (
  id        INTEGER PRIMARY KEY,
  book_id   INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  type      TEXT NOT NULL,                  -- chapter | event | character | plan | branch | promise | comment
  ref_table TEXT,                           -- на какую сущность ссылается
  ref_id    INTEGER,
  x         REAL DEFAULT 0,
  y         REAL DEFAULT 0,
  w         REAL,
  h         REAL,
  data      TEXT,                           -- JSON: поля, специфичные для типа
  collapsed INTEGER DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_node_book ON node(book_id);

CREATE TABLE IF NOT EXISTS edge (
  id        INTEGER PRIMARY KEY,
  book_id   INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  from_node INTEGER NOT NULL REFERENCES node(id) ON DELETE CASCADE,
  from_port TEXT,
  to_node   INTEGER NOT NULL REFERENCES node(id) ON DELETE CASCADE,
  to_port   TEXT,
  kind      TEXT DEFAULT 'flow',            -- flow (порядок глав) | data (участники, планы)
  label     TEXT
);

-- ─────────────────── ЗАМЕТКИ К ТЕКСТУ ───────────────────

CREATE TABLE IF NOT EXISTS annotation (
  id         INTEGER PRIMARY KEY,
  chapter_id INTEGER NOT NULL REFERENCES chapter(id) ON DELETE CASCADE,
  quote      TEXT NOT NULL,                 -- выделенный фрагмент
  prefix     TEXT,                          -- ~30 символов до (для повторной привязки)
  suffix     TEXT,                          -- ~30 символов после
  start_pos  INTEGER,
  end_pos    INTEGER,
  note       TEXT NOT NULL,
  status     TEXT DEFAULT 'открыта',        -- открыта | применена | отклонена | потеряна
  ai_result  TEXT,
  created_at TEXT DEFAULT (datetime('now')),
  resolved_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_annotation_chapter ON annotation(chapter_id, status);

-- ─────────────────── ИСТОРИЯ ГЕНЕРАЦИЙ ───────────────────

CREATE TABLE IF NOT EXISTS ai_run (
  id          INTEGER PRIMARY KEY,
  book_id     INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  chapter_id  INTEGER REFERENCES chapter(id) ON DELETE SET NULL,
  kind        TEXT,                          -- build_story | apply_notes | audit
  engine      TEXT,                          -- cli | api
  model       TEXT,
  prompt      TEXT,
  result      TEXT,
  actions_json TEXT,                       -- действия встроенного агента, без полного текста главы
  status      TEXT DEFAULT 'running',        -- running | ok | error | cancelled
  error       TEXT,
  tokens_in   INTEGER,
  tokens_out  INTEGER,
  created_at  TEXT DEFAULT (datetime('now')),
  finished_at TEXT
);

-- Если момент сдвинули по оси — подтягиваем скопированный порядок везде.
-- Объявлено в конце: триггер ссылается сразу на fact и knowledge.
CREATE TRIGGER IF NOT EXISTS moment_seq_upd AFTER UPDATE OF seq ON moment
BEGIN
  UPDATE fact      SET from_seq = NEW.seq WHERE from_moment = NEW.id;
  UPDATE fact      SET to_seq   = NEW.seq WHERE to_moment   = NEW.id;
  UPDATE knowledge SET from_seq = NEW.seq WHERE from_moment = NEW.id;
  UPDATE knowledge SET to_seq   = NEW.seq WHERE to_moment   = NEW.id;
END;

-- версии текста главы, чтобы можно было откатиться
CREATE TABLE IF NOT EXISTS chapter_version (
  id         INTEGER PRIMARY KEY,
  chapter_id INTEGER NOT NULL REFERENCES chapter(id) ON DELETE CASCADE,
  body       TEXT NOT NULL,
  reason     TEXT,                           -- ручная правка | build_story | apply_notes
  ai_run_id  INTEGER REFERENCES ai_run(id) ON DELETE SET NULL,
  created_at TEXT DEFAULT (datetime('now'))
);

-- Источники извлечённых утверждений и знание читателя по ходу книги.
CREATE TABLE IF NOT EXISTS reader_knowledge (
  id INTEGER PRIMARY KEY,
  book_id INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  topic TEXT NOT NULL,
  stance TEXT NOT NULL DEFAULT 'знает',
  belief TEXT,
  from_moment INTEGER REFERENCES moment(id) ON DELETE SET NULL,
  from_seq REAL,
  source_chapter_id INTEGER REFERENCES chapter(id) ON DELETE SET NULL,
  source_quote TEXT,
  source TEXT DEFAULT 'вручную',
  status TEXT DEFAULT 'канон'
);
CREATE INDEX IF NOT EXISTS idx_reader_knowledge_slice ON reader_knowledge(book_id, from_seq, topic);
CREATE TRIGGER IF NOT EXISTS reader_knowledge_seq_ins AFTER INSERT ON reader_knowledge BEGIN
  UPDATE reader_knowledge SET from_seq=(SELECT seq FROM moment WHERE id=NEW.from_moment) WHERE id=NEW.id;
END;
CREATE TRIGGER IF NOT EXISTS reader_knowledge_seq_upd AFTER UPDATE OF from_moment ON reader_knowledge BEGIN
  UPDATE reader_knowledge SET from_seq=(SELECT seq FROM moment WHERE id=NEW.from_moment) WHERE id=NEW.id;
END;
CREATE TRIGGER IF NOT EXISTS reader_moment_seq_upd AFTER UPDATE OF seq ON moment BEGIN
  UPDATE reader_knowledge SET from_seq=NEW.seq WHERE from_moment=NEW.id;
END;

-- Отобранные автором примеры стиля, отдельно от общего профиля.
CREATE TABLE IF NOT EXISTS style_example (
  id INTEGER PRIMARY KEY,
  book_id INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE,
  scene_type TEXT NOT NULL,
  title TEXT,
  body TEXT NOT NULL,
  source_chapter_id INTEGER REFERENCES chapter(id) ON DELETE SET NULL,
  created_at TEXT DEFAULT (datetime('now'))
);

-- Поиск по каноническому тексту глав. Индекс обновляется при сохранении и импорте.
CREATE VIRTUAL TABLE IF NOT EXISTS chapter_search USING fts5(
  book_id UNINDEXED, chapter_id UNINDEXED, body, tokenize='unicode61'
);
