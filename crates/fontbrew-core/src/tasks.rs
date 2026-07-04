use std::{collections::VecDeque, sync::Mutex, thread};

pub fn map_bounded<I, O, F>(inputs: Vec<I>, limit: usize, work: F) -> Vec<O>
where
    I: Send,
    O: Send,
    F: Fn(I) -> O + Sync,
{
    if inputs.is_empty() {
        return Vec::new();
    }

    let worker_count = limit.max(1).min(inputs.len());
    let queue = Mutex::new(inputs.into_iter().enumerate().collect::<VecDeque<_>>());
    let results = Mutex::new(Vec::new());
    let work = &work;

    thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| loop {
                let next_input = {
                    let mut queue = queue
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    queue.pop_front()
                };
                let Some((index, input)) = next_input else {
                    break;
                };

                let output = work(input);
                let mut results = results
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                results.push((index, output));
            });
        }
    });

    let mut results = results
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, output)| output).collect()
}
