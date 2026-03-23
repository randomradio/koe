#import <Foundation/Foundation.h>

@interface SPHistoryStats : NSObject

@property (nonatomic, assign) NSInteger sessionCount;
@property (nonatomic, assign) NSInteger totalDurationMs;
@property (nonatomic, assign) NSInteger totalCharCount;
@property (nonatomic, assign) NSInteger totalWordCount;

@end

@interface SPHistoryManager : NSObject

+ (instancetype)sharedManager;

- (void)recordSessionWithDurationMs:(NSInteger)durationMs
                               text:(NSString *)text;

- (void)recordUncertainPhrases:(NSArray<NSDictionary *> *)phrases;

- (NSArray<NSDictionary *> *)pendingUncertainPhrasesWithLimit:(NSInteger)limit;

- (void)resolveUncertainPhraseWithId:(NSInteger)phraseId
                      correctedPhrase:(NSString *)correctedPhrase;

- (void)ignoreUncertainPhraseWithId:(NSInteger)phraseId;

- (SPHistoryStats *)aggregateStats;

@end
