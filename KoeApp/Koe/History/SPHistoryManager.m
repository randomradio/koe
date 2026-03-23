#import "SPHistoryManager.h"
#import <sqlite3.h>

@implementation SPHistoryStats
@end

@interface SPHistoryManager () {
    sqlite3 *_db;
}
@end

@implementation SPHistoryManager

+ (instancetype)sharedManager {
    static SPHistoryManager *instance;
    static dispatch_once_t onceToken;
    dispatch_once(&onceToken, ^{
        instance = [[SPHistoryManager alloc] init];
    });
    return instance;
}

- (instancetype)init {
    self = [super init];
    if (self) {
        [self openDatabase];
    }
    return self;
}

- (void)openDatabase {
    NSString *dir = [NSString stringWithFormat:@"%@/.koe", NSHomeDirectory()];
    [[NSFileManager defaultManager] createDirectoryAtPath:dir
                              withIntermediateDirectories:YES
                                               attributes:nil
                                                    error:nil];
    NSString *dbPath = [dir stringByAppendingPathComponent:@"history.db"];

    if (sqlite3_open(dbPath.UTF8String, &_db) != SQLITE_OK) {
        NSLog(@"[Koe] Failed to open history database: %s", sqlite3_errmsg(_db));
        _db = NULL;
        return;
    }

    const char *sql =
        "CREATE TABLE IF NOT EXISTS sessions ("
        "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
        "  timestamp INTEGER NOT NULL,"
        "  duration_ms INTEGER NOT NULL,"
        "  text TEXT NOT NULL,"
        "  char_count INTEGER NOT NULL,"
        "  word_count INTEGER NOT NULL"
        ");";

    char *errMsg = NULL;
    if (sqlite3_exec(_db, sql, NULL, NULL, &errMsg) != SQLITE_OK) {
        NSLog(@"[Koe] Failed to create sessions table: %s", errMsg);
        sqlite3_free(errMsg);
    }

    const char *uncertainSql =
        "CREATE TABLE IF NOT EXISTS uncertain_phrases ("
        "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
        "  timestamp INTEGER NOT NULL,"
        "  session_id TEXT,"
        "  phrase TEXT NOT NULL,"
        "  suggestion TEXT,"
        "  reason TEXT,"
        "  confidence REAL,"
        "  context_text TEXT,"
        "  status TEXT NOT NULL DEFAULT 'new',"
        "  corrected_phrase TEXT"
        ");";

    errMsg = NULL;
    if (sqlite3_exec(_db, uncertainSql, NULL, NULL, &errMsg) != SQLITE_OK) {
        NSLog(@"[Koe] Failed to create uncertain_phrases table: %s", errMsg);
        sqlite3_free(errMsg);
    }

    const char *indexSql =
        "CREATE INDEX IF NOT EXISTS idx_uncertain_phrases_status_timestamp "
        "ON uncertain_phrases(status, timestamp DESC);";
    errMsg = NULL;
    if (sqlite3_exec(_db, indexSql, NULL, NULL, &errMsg) != SQLITE_OK) {
        NSLog(@"[Koe] Failed to create uncertain_phrases index: %s", errMsg);
        sqlite3_free(errMsg);
    }
}

- (void)recordSessionWithDurationMs:(NSInteger)durationMs
                               text:(NSString *)text {
    if (!_db) return;
    NSString *safeText = text ?: @"";

    NSInteger charCount = 0;
    NSInteger wordCount = 0;
    [self countText:safeText charCount:&charCount wordCount:&wordCount];

    const char *sql = "INSERT INTO sessions (timestamp, duration_ms, text, char_count, word_count) "
                      "VALUES (?, ?, ?, ?, ?);";
    sqlite3_stmt *stmt = NULL;

    if (sqlite3_prepare_v2(_db, sql, -1, &stmt, NULL) == SQLITE_OK) {
        sqlite3_bind_int64(stmt, 1, (sqlite3_int64)[[NSDate date] timeIntervalSince1970]);
        sqlite3_bind_int64(stmt, 2, (sqlite3_int64)durationMs);
        sqlite3_bind_text(stmt, 3, safeText.UTF8String, -1, SQLITE_TRANSIENT);
        sqlite3_bind_int64(stmt, 4, (sqlite3_int64)charCount);
        sqlite3_bind_int64(stmt, 5, (sqlite3_int64)wordCount);

        if (sqlite3_step(stmt) != SQLITE_DONE) {
            NSLog(@"[Koe] Failed to insert session: %s", sqlite3_errmsg(_db));
        }
    }
    sqlite3_finalize(stmt);

    NSLog(@"[Koe] History recorded — duration:%ldms chars:%ld words:%ld",
          (long)durationMs, (long)charCount, (long)wordCount);
}

- (void)recordUncertainPhrases:(NSArray<NSDictionary *> *)phrases {
    if (!_db || phrases.count == 0) return;

    const char *sql =
        "INSERT INTO uncertain_phrases ("
        "  timestamp, session_id, phrase, suggestion, reason, confidence, context_text, status"
        ") VALUES (?, ?, ?, ?, ?, ?, ?, ?);";

    sqlite3_stmt *stmt = NULL;
    if (sqlite3_prepare_v2(_db, sql, -1, &stmt, NULL) != SQLITE_OK) {
        NSLog(@"[Koe] Failed to prepare uncertain phrase insert: %s", sqlite3_errmsg(_db));
        return;
    }

    NSInteger inserted = 0;
    for (NSDictionary *item in phrases) {
        NSString *phrase = [item[@"phrase"] isKindOfClass:[NSString class]] ? item[@"phrase"] : @"";
        phrase = [phrase stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceAndNewlineCharacterSet]];
        if (phrase.length == 0) continue;

        NSString *sessionId = [item[@"session_id"] isKindOfClass:[NSString class]] ? item[@"session_id"] : nil;
        NSString *suggestion = [item[@"suggestion"] isKindOfClass:[NSString class]] ? item[@"suggestion"] : @"";
        NSString *reason = [item[@"reason"] isKindOfClass:[NSString class]] ? item[@"reason"] : @"";
        NSString *contextText = [item[@"context_text"] isKindOfClass:[NSString class]] ? item[@"context_text"] : @"";

        double confidence = 0.5;
        id confidenceValue = item[@"confidence"];
        if ([confidenceValue isKindOfClass:[NSNumber class]]) {
            confidence = [confidenceValue doubleValue];
        } else if ([confidenceValue isKindOfClass:[NSString class]]) {
            confidence = [confidenceValue doubleValue];
        }
        if (confidence < 0.0) confidence = 0.0;
        if (confidence > 1.0) confidence = 1.0;

        sqlite3_reset(stmt);
        sqlite3_clear_bindings(stmt);

        sqlite3_bind_int64(stmt, 1, (sqlite3_int64)[[NSDate date] timeIntervalSince1970]);
        if (sessionId.length > 0) {
            sqlite3_bind_text(stmt, 2, sessionId.UTF8String, -1, SQLITE_TRANSIENT);
        } else {
            sqlite3_bind_null(stmt, 2);
        }
        sqlite3_bind_text(stmt, 3, phrase.UTF8String, -1, SQLITE_TRANSIENT);
        sqlite3_bind_text(stmt, 4, suggestion.UTF8String, -1, SQLITE_TRANSIENT);
        sqlite3_bind_text(stmt, 5, reason.UTF8String, -1, SQLITE_TRANSIENT);
        sqlite3_bind_double(stmt, 6, confidence);
        sqlite3_bind_text(stmt, 7, contextText.UTF8String, -1, SQLITE_TRANSIENT);
        sqlite3_bind_text(stmt, 8, "new", -1, SQLITE_TRANSIENT);

        if (sqlite3_step(stmt) == SQLITE_DONE) {
            inserted++;
        } else {
            NSLog(@"[Koe] Failed to insert uncertain phrase: %s", sqlite3_errmsg(_db));
        }
    }

    sqlite3_finalize(stmt);

    if (inserted > 0) {
        NSLog(@"[Koe] Stored %ld uncertain phrase candidates", (long)inserted);
    }
}

- (NSArray<NSDictionary *> *)pendingUncertainPhrasesWithLimit:(NSInteger)limit {
    if (!_db) return @[];

    NSInteger effectiveLimit = limit > 0 ? limit : 20;
    NSString *sql = [NSString stringWithFormat:
                     @"SELECT id, timestamp, session_id, phrase, suggestion, reason, confidence, context_text, status, corrected_phrase "
                     "FROM uncertain_phrases "
                     "WHERE status = 'new' "
                     "ORDER BY timestamp DESC, id DESC "
                     "LIMIT %ld;", (long)effectiveLimit];

    sqlite3_stmt *stmt = NULL;
    NSMutableArray<NSDictionary *> *rows = [NSMutableArray array];

    if (sqlite3_prepare_v2(_db, sql.UTF8String, -1, &stmt, NULL) == SQLITE_OK) {
        while (sqlite3_step(stmt) == SQLITE_ROW) {
            int64_t phraseId = sqlite3_column_int64(stmt, 0);
            int64_t timestamp = sqlite3_column_int64(stmt, 1);

            const unsigned char *sessionIdC = sqlite3_column_text(stmt, 2);
            const unsigned char *phraseC = sqlite3_column_text(stmt, 3);
            const unsigned char *suggestionC = sqlite3_column_text(stmt, 4);
            const unsigned char *reasonC = sqlite3_column_text(stmt, 5);
            double confidence = sqlite3_column_double(stmt, 6);
            const unsigned char *contextC = sqlite3_column_text(stmt, 7);
            const unsigned char *statusC = sqlite3_column_text(stmt, 8);
            const unsigned char *correctedC = sqlite3_column_text(stmt, 9);

            NSString *sessionId = sessionIdC ? [NSString stringWithUTF8String:(const char *)sessionIdC] : @"";
            NSString *phrase = phraseC ? [NSString stringWithUTF8String:(const char *)phraseC] : @"";
            NSString *suggestion = suggestionC ? [NSString stringWithUTF8String:(const char *)suggestionC] : @"";
            NSString *reason = reasonC ? [NSString stringWithUTF8String:(const char *)reasonC] : @"";
            NSString *contextText = contextC ? [NSString stringWithUTF8String:(const char *)contextC] : @"";
            NSString *status = statusC ? [NSString stringWithUTF8String:(const char *)statusC] : @"";
            NSString *corrected = correctedC ? [NSString stringWithUTF8String:(const char *)correctedC] : @"";

            NSDictionary *row = @{
                @"id": @(phraseId),
                @"timestamp": @(timestamp),
                @"session_id": sessionId,
                @"phrase": phrase,
                @"suggestion": suggestion,
                @"reason": reason,
                @"confidence": @(confidence),
                @"context_text": contextText,
                @"status": status,
                @"corrected_phrase": corrected,
            };
            [rows addObject:row];
        }
    } else {
        NSLog(@"[Koe] Failed to query pending uncertain phrases: %s", sqlite3_errmsg(_db));
    }

    sqlite3_finalize(stmt);
    return rows;
}

- (void)resolveUncertainPhraseWithId:(NSInteger)phraseId
                      correctedPhrase:(NSString *)correctedPhrase {
    if (!_db || phraseId <= 0) return;

    NSString *trimmed = [correctedPhrase stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceAndNewlineCharacterSet]];
    if (trimmed.length == 0) return;

    const char *sql = "UPDATE uncertain_phrases SET status = 'resolved', corrected_phrase = ? WHERE id = ?;";
    sqlite3_stmt *stmt = NULL;
    if (sqlite3_prepare_v2(_db, sql, -1, &stmt, NULL) == SQLITE_OK) {
        sqlite3_bind_text(stmt, 1, trimmed.UTF8String, -1, SQLITE_TRANSIENT);
        sqlite3_bind_int64(stmt, 2, (sqlite3_int64)phraseId);
        if (sqlite3_step(stmt) != SQLITE_DONE) {
            NSLog(@"[Koe] Failed to resolve uncertain phrase #%ld: %s", (long)phraseId, sqlite3_errmsg(_db));
        }
    } else {
        NSLog(@"[Koe] Failed to prepare resolve uncertain phrase #%ld: %s", (long)phraseId, sqlite3_errmsg(_db));
    }
    sqlite3_finalize(stmt);
}

- (void)ignoreUncertainPhraseWithId:(NSInteger)phraseId {
    if (!_db || phraseId <= 0) return;

    const char *sql = "UPDATE uncertain_phrases SET status = 'ignored' WHERE id = ?;";
    sqlite3_stmt *stmt = NULL;
    if (sqlite3_prepare_v2(_db, sql, -1, &stmt, NULL) == SQLITE_OK) {
        sqlite3_bind_int64(stmt, 1, (sqlite3_int64)phraseId);
        if (sqlite3_step(stmt) != SQLITE_DONE) {
            NSLog(@"[Koe] Failed to ignore uncertain phrase #%ld: %s", (long)phraseId, sqlite3_errmsg(_db));
        }
    } else {
        NSLog(@"[Koe] Failed to prepare ignore uncertain phrase #%ld: %s", (long)phraseId, sqlite3_errmsg(_db));
    }
    sqlite3_finalize(stmt);
}

- (void)countText:(NSString *)text charCount:(NSInteger *)outChars wordCount:(NSInteger *)outWords {
    NSInteger chars = 0;
    NSInteger words = 0;
    BOOL inWord = NO;

    for (NSUInteger i = 0; i < text.length; i++) {
        unichar ch = [text characterAtIndex:i];

        // CJK Unified Ideographs and extensions
        if ((ch >= 0x4E00 && ch <= 0x9FFF) ||   // CJK Unified
            (ch >= 0x3400 && ch <= 0x4DBF) ||   // CJK Extension A
            (ch >= 0xF900 && ch <= 0xFAFF)) {   // CJK Compatibility
            chars++;
            if (inWord) {
                words++;
                inWord = NO;
            }
        } else if ((ch >= 'A' && ch <= 'Z') || (ch >= 'a' && ch <= 'z') ||
                   (ch >= '0' && ch <= '9') || ch == '\'') {
            // Latin alphanumeric — part of a word
            if (!inWord) {
                inWord = YES;
            }
        } else {
            // Whitespace, punctuation, etc.
            if (inWord) {
                words++;
                inWord = NO;
            }
        }
    }
    if (inWord) {
        words++;
    }

    *outChars = chars;
    *outWords = words;
}

- (SPHistoryStats *)aggregateStats {
    SPHistoryStats *stats = [[SPHistoryStats alloc] init];
    if (!_db) return stats;

    const char *sql = "SELECT COUNT(*), COALESCE(SUM(duration_ms),0), "
                      "COALESCE(SUM(char_count),0), COALESCE(SUM(word_count),0) "
                      "FROM sessions;";
    sqlite3_stmt *stmt = NULL;

    if (sqlite3_prepare_v2(_db, sql, -1, &stmt, NULL) == SQLITE_OK) {
        if (sqlite3_step(stmt) == SQLITE_ROW) {
            stats.sessionCount = (NSInteger)sqlite3_column_int64(stmt, 0);
            stats.totalDurationMs = (NSInteger)sqlite3_column_int64(stmt, 1);
            stats.totalCharCount = (NSInteger)sqlite3_column_int64(stmt, 2);
            stats.totalWordCount = (NSInteger)sqlite3_column_int64(stmt, 3);
        }
    }
    sqlite3_finalize(stmt);

    return stats;
}

- (void)dealloc {
    if (_db) {
        sqlite3_close(_db);
        _db = NULL;
    }
}

@end
