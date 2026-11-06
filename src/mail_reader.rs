//! 邮件全文阅读窗口：完整正文、收件账号和网页链接。
use crate::app::{App,Cmd};
use crate::mail_format::{self,Kind};
use eframe::egui::{self,Color32};

/// 正文按块渲染：标题加粗放大、列表带符号缩进、引用变灰、分隔线画细线。
pub(crate) fn body_ui(ui:&mut egui::Ui,lines:&[mail_format::Line],base:f32){
    ui.spacing_mut().item_spacing.y=2.0;
    ui.spacing_mut().indent=16.0;
    for (index,line) in lines.iter().enumerate() {
        match line.kind {
            Kind::Title=>{
                ui.add_space(6.0);
                ui.add(egui::Label::new(egui::RichText::new(&line.text).size(base+4.0).strong().color(Color32::WHITE)).wrap());
            },
            Kind::Heading=>{
                ui.add_space(8.0);
                ui.add(egui::Label::new(egui::RichText::new(&line.text).size(base+2.0).strong().color(Color32::from_gray(248))).wrap());
            },
            Kind::Body=>{
                ui.add(egui::Label::new(egui::RichText::new(&line.text).size(base).color(Color32::from_gray(232))).wrap());
            },
            Kind::Bullet=>{
                ui.indent(("mail-bullet",index),|ui|{
                    ui.add(egui::Label::new(egui::RichText::new(format!("•  {}",line.text)).size(base).color(Color32::from_gray(232))).wrap());
                });
            },
            Kind::Quote=>{
                ui.add_space(3.0);
                ui.indent(("mail-quote",index),|ui|{
                    ui.add(egui::Label::new(egui::RichText::new(format!("│  {}",line.text)).size(base-0.5).color(Color32::from_gray(176))).wrap());
                });
            },
            Kind::Gap=>ui.add_space(7.0),
            Kind::Divider=>{
                ui.add_space(5.0);
                ui.separator();
                ui.add_space(5.0);
            },
        }
    }
}

impl App {
    pub fn mail_reader_ui(&mut self,ctx:&egui::Context) {
        let Some(raw)=self.mail.reading.clone() else{return};
        let hidden=self.mail_code_hidden(&raw);
        let item=self.display_mail(&raw);
        let body=mail_format::item_lines(&item);
        let mut reveal=false;
        let focus=std::mem::take(&mut self.mail.reader_focus);
        let (mut close,mut link)=(false,None);
        let mut toggle=None;
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("daozai-mail-reader"),
            egui::ViewportBuilder::default().with_title("邮件全文 · 岛仔").with_inner_size([700.0,620.0]).with_min_inner_size([500.0,380.0]),|ctx,_|{
            if focus{ctx.send_viewport_cmd(egui::ViewportCommand::Focus);}
            close=ctx.input(|i|i.viewport().close_requested());
            if raw.code.is_some(){ctx.request_repaint_after(std::time::Duration::from_secs(1));}
            egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::from_rgb(25,25,27)).inner_margin(22)).show(ctx,|ui|{
                ui.spacing_mut().item_spacing.y=8.0;
                ui.add(egui::Label::new(egui::RichText::new(&item.subject).size(20.0).strong().color(Color32::WHITE)).wrap());
                egui::Grid::new("mail-reader-meta").spacing([12.0,5.0]).show(ui,|ui|{
                    for (label,text) in [("发件人",&item.from),("收件邮箱",&item.account),("时间",&item.time)]{
                        ui.label(egui::RichText::new(label).color(Color32::from_gray(160)));
                        ui.label(egui::RichText::new(text).color(Color32::from_gray(220)));ui.end_row();
                    }
                });
                if let Some(event)=&item.meeting {
                    ui.horizontal_wrapped(|ui|{
                        ui.label(format!("日程 · {} · {}",event.at,event.title));
                        if ui.button(if self.mail_event_ignored(event){"恢复日程"}else{"忽略日程"}).clicked(){toggle=Some(event.clone());}
                    });
                }
                if hidden {
                    ui.label("验证码已隐藏，不代表验证码失效；相关链接也暂时隐藏。");
                    reveal=ui.button("主动查看验证码与正文 15 秒").clicked();
                }
                ui.separator();
                ui.label(egui::RichText::new("鼠标滚轮或拖动右侧滚动条查看全文").size(11.5).color(Color32::from_gray(160)));
                ui.style_mut().spacing.scroll.floating=false;
                egui::ScrollArea::vertical().id_salt(("mail-reader-body",&item.account,item.uid)).auto_shrink([false,false]).show(ui,|ui|{
                    if body.is_empty(){
                        ui.label(egui::RichText::new("（这封邮件没有正文）").size(14.0).color(Color32::from_gray(200)));
                    }else{
                        body_ui(ui,&body,14.0);
                    }
                    if !item.links.is_empty(){
                        ui.add_space(12.0);ui.separator();ui.label(egui::RichText::new("邮件中的链接").strong());
                        for target in &item.links{
                            if ui.link(&target.label).on_hover_text(&target.url).clicked(){link=Some(target.url.clone());}
                        }
                    }
                });
            });
        });
        if reveal{self.reveal_mail_code(&raw);ctx.request_repaint();}
        if let Some(event)=toggle{self.toggle_mail_event(&event);}
        if close{self.mail.reading=None;}
        if let Some(url)=link{self.pending.push(Cmd::OpenUrl(url));ctx.request_repaint();}
    }
}
