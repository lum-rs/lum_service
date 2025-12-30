use core::mem;
use std::future::Future;

use lum_boxtypes::LifetimedPinnedBoxedFuture;

//TODO: Remove Sync bound
pub struct Taskchain<'task, T: Send + 'task> {
    task: LifetimedPinnedBoxedFuture<'task, T>,
}

impl<'task, T: Send + 'task> Taskchain<'task, T> {
    pub fn new(task: LifetimedPinnedBoxedFuture<'task, T>) -> Self {
        Self { task }
    }

    pub fn append<FN, FUT>(&mut self, task: FN)
    where
        FN: FnOnce(T) -> FUT + Send + 'task,
        FUT: Future<Output = T> + Send,
    {
        let previous_task = mem::replace(
            &mut self.task,
            Box::pin(async { unreachable!("Undefined Taskchain task") }),
        );

        let task = async move {
            let result = previous_task.await;
            task(result).await
        };

        self.task = Box::pin(task);
    }

    pub async fn run(self) -> T {
        self.task.await
    }
}
